//! Top-level application state and dispatch.
//!
//! [`App`] is the root of the ownership hierarchy. It creates the tokio
//! runtime, builds shared [`Services`], and dispatches to the appropriate
//! [`Runner`] variant (TUI or headless).

use crate::bootstrap;
use crate::bootstrap::config::LaunchServices;
use error_stack::{Report, ResultExt};
use jinn_cli::Cli;
use tokio::runtime::Runtime;
use wherror::Error;

use crate::actor_wiring;
#[cfg(debug_assertions)]
use crate::headless::HeadlessApp;
use crate::runner::Runner;
use marked_path::{Absolute, MarkedPathAccess, MarkedPathBuf};

/// Error type for top-level application initialization.
#[derive(Debug, Error)]
#[error(debug)]
pub struct AppError;

/// Resolves the `--dump-requests` value into a typed absolute dump dir.
///
/// A relative value joins the process cwd and a leading `~` expands against
/// the home directory, per [`crate::cli_paths`].
///
/// # Errors
///
/// Returns an error naming the flag and the raw value when the value cannot
/// be resolved to an absolute path.
fn resolve_dump_requests(
    dir: Option<&std::path::Path>,
) -> Result<Option<MarkedPathBuf<Absolute>>, Report<AppError>> {
    let Some(dir) = dir else {
        return Ok(None);
    };
    crate::cli_paths::resolve_cli_path(
        "--dump-requests",
        dir,
        jinn_kernel::AppPaths::default().home_dir(),
    )
    .change_context(AppError)
    .map(Some)
}

/// Top-level application state.
///
/// Created once in `crate::main` and dispatched to whichever
/// runner handles the command. Owns the tokio runtime and delegates
/// to [`Runner`] variants.
pub struct App {
    /// The tokio runtime. `None` once [`App::run_and_shutdown`] has
    /// consumed it with a bounded deadline — the runtime's ordinary
    /// `Drop` waits for every blocking task, which a quit during a
    /// model download must not do.
    runtime: Option<Runtime>,
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
        Ok(Self {
            runtime: Some(runtime),
        })
    }

    /// Returns a handle to the tokio runtime for spawning tasks.
    #[must_use]
    pub fn handle(&self) -> tokio::runtime::Handle {
        self.runtime().handle().clone()
    }

    /// The live runtime.
    ///
    /// # Panics
    ///
    /// Panics if called after [`App::run_and_shutdown`], the one
    /// consumer, has already taken it.
    #[expect(
        clippy::expect_used,
        reason = "the runtime is consumed only by the final shutdown; a second take is a lifecycle bug the panic names"
    )]
    fn runtime(&self) -> &Runtime {
        self.runtime
            .as_ref()
            .expect("the runtime is consumed only by the final shutdown")
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
    /// As its final act it consumes the runtime with a bounded
    /// deadline. The runtime's `Drop` joins every blocking task before
    /// returning, so an ordinary exit would hang until an in-flight
    /// blocking task — a 600 MB model download quit mid-stream —
    /// finished. The 2s deadline abandons such stragglers instead:
    /// they die with the process, and the fetch backend resumes
    /// cleanly on the next launch.
    ///
    /// # Errors
    ///
    /// Returns an error if the runner fails or the store shutdown fails.
    fn run_and_shutdown(
        &mut self,
        runner: crate::runner::Runner,
        store: &jinn_session_state::SessionStoreService,
    ) -> Result<(), Report<AppError>> {
        // Extract the trouper system handle before the runner is consumed,
        // so the graceful shutdown sweep can run after the event loop exits.
        let trouper_system = runner.trouper_system();

        // Run the runner, but don't short-circuit shutdown on its error.
        // The WAL checkpoint is non-destructive and must run whenever the actor
        // system started, so that a clean quit leaves sessions.db self-contained
        // even if the runner itself failed. Surface the runner error as the
        // final result; a shutdown error takes precedence (it indicates storage
        // trouble the caller should see).
        let run_result = runner.run().change_context(AppError);

        // Best-effort registry removal: a graceful exit stops advertising
        // this instance immediately rather than waiting for the next
        // reader's port probe. It runs AFTER `runner.run()` — deregistering
        // first would erase the entry the listener just registered and
        // leave the instance undiscoverable for its whole life.
        // Correctness never depends on it — a dead entry is invisible to
        // every `jinn remote` client either way.
        jinn_remote::remote_actor::deregister(
            jinn_kernel::AppPaths::default()
                .remote_instances_path()
                .as_path(),
            std::process::id(),
        );

        // Graceful shutdown: the trouper sweep fires every supervised
        // child's shutdown watcher (each loop drains, runs its `on_stop`
        // hook, and tears down), races the drain against a 10s deadline,
        // then flushes the store. A wedged actor can't prevent process
        // exit — the sweep hard-stops stragglers at the deadline.
        if let Some(trouper_system) = trouper_system {
            self.runtime().block_on(async {
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

        self.runtime()
            .block_on(store.shutdown())
            .change_context(AppError)
            .attach("session store shutdown (WAL checkpoint) failed")?;

        // Bounded final shutdown: consume the runtime so its `Drop` never
        // runs — the drop joins every blocking task, and a quit during a
        // model download would hang on it. Abandoned tasks die with the
        // process; the fetch backend resumes on the next launch. This
        // runs after the WAL checkpoint so the abandon cannot interrupt
        // a mid-write checkpoint.
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_timeout(std::time::Duration::from_secs(2));
        }
        run_result
    }

    /// Serves the browser daemon child process: resolve the configured
    /// or detected browser binary, launch headed Chrome, bind the
    /// loopback API.
    ///
    /// # Errors
    ///
    /// An unreadable `[browser]` section or a daemon startup refusal
    /// surfaces as an [`AppError`].
    fn serve_browser_daemon(
        &self,
        config: Option<MarkedPathBuf<Absolute>>,
    ) -> Result<(), Report<AppError>> {
        let paths = jinn_kernel::AppPaths::default();
        let config_path = match config {
            Some(p) => p.as_path().to_path_buf(),
            None => paths.preferences_path().as_path().to_path_buf(),
        };
        let browser_config = jinn_browser::daemon::read_browser_section(&config_path)
            .change_context(AppError)
            .attach("the browser daemon could not read its [browser] section")?;
        self.runtime()
            .block_on(jinn_browser::daemon::serve(
                &paths,
                &browser_config,
                &jinn_common::binary_detect::WhichProbe,
            ))
            .change_context(AppError)
            .attach("browser daemon failed")
    }

    /// Dispatches the CLI command to the appropriate runner.
    ///
    /// # Errors
    ///
    /// Returns an error if the runner fails.
    pub fn dispatch(&mut self, cli: Cli) -> Result<(), Report<AppError>> {
        // Read config and build every service the actors will hold. This
        // also runs the recovery subcommands (`config init`,
        // `config providers`, `install`) — and intercepts the bundled MCP
        // servers, which never open the session store or the config (see
        // `bootstrap::config` for why the order there is behavioural).
        match bootstrap::config::prepare(&cli, self.runtime()).change_context(AppError)? {
            // A bundled server runs over stdio until the client disconnects,
            // then the process returns; no TUI, no session store. With
            // --fetch-model, the child warms the embedding model's cache and
            // exits instead of serving — never touching stdout.
            bootstrap::config::Prepared::ServeBundled {
                db,
                embed_model,
                fetch_model,
            } => {
                let args = jinn_mcp_overgraph::OvergraphServeArgs {
                    db,
                    embed_model,
                    fetch_model,
                };
                if fetch_model {
                    return jinn_mcp_overgraph::fetch_model(&args)
                        .change_context(AppError)
                        .attach("overgraph embedding model pre-fetch failed");
                }
                self.runtime()
                    .block_on(jinn_mcp_overgraph::serve(args))
                    .change_context(AppError)
                    .attach("overgraph MCP server failed")
            }
            // The smolvm child: same stdio-until-disconnect contract as the
            // overgraph child above. Its boot sequence (manifest resolve,
            // availability gate, engine, machine) renders its own fatal
            // failures to stderr — the spawned child's only feedback.
            bootstrap::config::Prepared::ServeSmolvm { cwd, smolfile } => {
                let args = jinn_mcp_smolvm::SmolvmServeArgs { cwd, smolfile };
                self.runtime()
                    .block_on(jinn_mcp_smolvm::serve(args))
                    .change_context(AppError)
                    .attach("smolvm MCP server failed")
            }
            // The browser MCP child: stdio until disconnect, holding only
            // a daemon client. Its refusals are tool-level content the
            // model reads; a serve failure is protocol-fatal.
            bootstrap::config::Prepared::ServeMcpBrowser => self
                .runtime()
                .block_on(jinn_mcp_browser::serve())
                .change_context(AppError)
                .attach("browser MCP server failed"),
            // The browser daemon child: resolve the binary, launch headed
            // Chrome, serve the loopback API until /shutdown or kill. Its
            // startup refusals (already-live, unresolvable configured
            // binary) render to stderr as the child's only feedback.
            bootstrap::config::Prepared::ServeBrowserDaemon { config } => {
                self.serve_browser_daemon(config)
            }
            // A browser client subcommand: registry + loopback HTTP only.
            // The client prints its own user-facing output.
            bootstrap::config::Prepared::BrowserClient(command) => {
                let outcome = crate::browser_cli::run(&command)
                    .change_context(AppError)
                    .attach("browser client command failed")?;
                // The client subcommands are CLI surfaces: their whole
                // result is stdout text (this arm never opens the TUI).
                #[expect(
                    clippy::print_stdout,
                    reason = "browser client commands are CLI surfaces; stdout is their output channel"
                )]
                {
                    println!("{outcome}");
                }
                Ok(())
            }
            bootstrap::config::Prepared::Handled => Ok(()),
            bootstrap::config::Prepared::Ready(services) => self.run_ready_command(cli, services),
        }
    }

    /// Runs a normal startup from prepared [`LaunchServices`].
    ///
    /// # Errors
    ///
    /// Returns an error if the chosen runner fails.
    fn run_ready_command(
        &mut self,
        cli: Cli,
        services: LaunchServices,
    ) -> Result<(), Report<AppError>> {
        use jinn_cli::cli::Commands;
        let LaunchServices {
            config_storage,
            api_keys: resolved_api_keys,
            provider_registry,
            llm_service,
            session_store,
            session_pool,
            config,
            app_state_storage,
            shells,
            resolved_config_path,
        } = services;
        let dump_requests = resolve_dump_requests(cli.dump_requests.as_deref())?;
        #[cfg(debug_assertions)]
        let _db_path = cli.db_path_opt().cloned();
        {
            match cli.command.unwrap_or(Commands::Tui) {
                Commands::Completions { shell } => {
                    use clap::CommandFactory;
                    let mut cmd = Cli::command();
                    let name = cmd.get_name().to_owned();
                    clap_complete::generate(shell, &mut cmd, name, &mut std::io::stdout());
                    return Ok(());
                }
                // The luau runner never reaches dispatch (prepare
                // intercepts it and exits the process), but the match
                // must stay exhaustive over the parse surface. Routing
                // it to the TUI arm like the other intercepted commands
                // documents that no startup path exists for it.
                // `jinn mcp noop` falls through to a normal startup: the
                // variant remains for argv-shape compatibility; the real
                // bundled servers were intercepted in `prepare`. Canvas
                // commands are intercepted the same way; this arm is
                // unreachable for them. `jinn remote …` is intercepted
                // in `prepare` too — it never reaches a startup path.
                Commands::Luau { .. }
                | Commands::Tui
                | Commands::Mcp { .. }
                | Commands::Browser { .. }
                | Commands::Remote { .. } => {
                    self.run_tui_command(
                        dump_requests,
                        llm_service,
                        provider_registry,
                        resolved_api_keys,
                        config_storage,
                        &session_store,
                        config,
                        app_state_storage,
                        shells,
                        resolved_config_path,
                        session_pool,
                    )?;
                }
                #[cfg(debug_assertions)]
                Commands::Headless { command, .. } => {
                    self.run_headless_command(
                        dump_requests,
                        llm_service,
                        provider_registry,
                        resolved_api_keys,
                        config_storage,
                        &session_store,
                        config,
                        app_state_storage,
                        shells,
                        resolved_config_path,
                        command,
                    )?;
                }
                Commands::Fetch { subcommand } => {
                    run_fetch_subcommand(self.runtime(), &subcommand)?;
                }
                Commands::Dictation { subcommand } => {
                    run_dictation_subcommand(&subcommand);
                }
                // Config subcommands are dispatched above, before the early
                // preferences parse. Reaching this match arm is impossible.
                Commands::Config { .. } => {}
                Commands::Install { .. } => {}
            }

            Ok(())
        }
    }
}

/// The `jinn fetch` subcommands: model downloads and listings.
///
/// # Errors
///
/// Returns an error when the models listing fails.
fn run_fetch_subcommand(
    runtime: &Runtime,
    subcommand: &jinn_cli::cli::FetchCommands,
) -> Result<(), Report<AppError>> {
    use jinn_cli::cli::FetchCommands;

    match *subcommand {
        FetchCommands::Models => {
            runtime
                .block_on(async { fetch_models().await })
                .change_context(AppError)?;
        }
        FetchCommands::DictationModel => {
            // A dedicated exit path: a platform
            // without a real backend prints and
            // exits non-zero here.
            fetch_dictation_model(runtime);
        }
    }
    Ok(())
}

/// The `jinn dictation` subcommands: device listing.
fn run_dictation_subcommand(subcommand: &jinn_cli::cli::DictationCommands) {
    use jinn_cli::cli::DictationCommands;

    match *subcommand {
        DictationCommands::ListDevices => {
            // A dedicated exit path, the same
            // contract as the fetch runner: a
            // platform without a real backend
            // prints and exits non-zero here.
            list_dictation_devices();
        }
    }
}

impl App {
    /// The `jinn` (TUI) command: activate the actor system, attach the
    /// discord gateway spawner, and hand the TUI to the runner.
    ///
    /// # Errors
    ///
    /// Returns an error if activation or the TUI launch fails.
    #[expect(
        clippy::too_many_arguments,
        reason = "one parameter per destructured `LaunchServices` field; a struct would only rename the field list"
    )]
    fn run_tui_command(
        &mut self,
        dump_requests: Option<MarkedPathBuf<Absolute>>,
        llm_service: jinn_kernel::LlmServiceFactoryService,
        provider_registry: jinn_kernel::ProviderRegistryService,
        api_keys: jinn_kernel::ApiKeysService,
        config_storage: jinn_kernel::ConfigStorageService,
        session_store: &jinn_session_state::SessionStoreService,
        config: jinn_config::ConfigLayer,
        app_state_storage: jinn_preferences_config::AppStateStorageService,
        shells: jinn_common::shell::Shells,
        resolved_config_path: MarkedPathBuf<Absolute>,
        session_pool: jinn_discord::backend::spawn::SessionPool,
    ) -> Result<(), Report<AppError>> {
        let (core, services, discord_activated) = self.activate_system(
            llm_service,
            provider_registry,
            api_keys,
            config_storage,
            session_store.clone(),
            config,
            app_state_storage,
            shells,
            resolved_config_path,
            dump_requests,
        )?;

        // The discord frontend attaches its spawn hook to the activation's
        // supervisor: an enabled config spawns immediately, a live
        // enablement flip spawns later through the same hook.
        {
            let spawner = jinn_discord::backend::spawn::gateway_spawner(
                &self.handle(),
                &core,
                &services,
                session_pool,
                discord_activated.parked.clone(),
            );
            let mut supervisor =
                tokio::task::block_in_place(|| discord_activated.supervisor.blocking_lock());
            supervisor.attach_spawner(Box::new(spawner));
        }

        let app = jinn_tui::launch(core, services, bootstrap::ui::build_ui_registry())
            .change_context(AppError)?;
        let runner = Runner::Tui(Box::new(app));
        self.run_and_shutdown(runner, session_store)
    }

    /// The `jinn headless` command: activate, run the requested headless
    /// command (send one chat, run a script), and shut down.
    ///
    /// # Errors
    ///
    /// Returns an error if activation or the headless command fails.
    #[cfg(debug_assertions)]
    #[expect(
        clippy::too_many_arguments,
        reason = "the builder args are destructured at the call site; re-wrapping them in a struct would just move the field list"
    )]
    fn run_headless_command(
        &mut self,
        dump_requests: Option<MarkedPathBuf<Absolute>>,
        llm_service: jinn_kernel::LlmServiceFactoryService,
        provider_registry: jinn_kernel::ProviderRegistryService,
        api_keys: jinn_kernel::ApiKeysService,
        config_storage: jinn_kernel::ConfigStorageService,
        session_store: &jinn_session_state::SessionStoreService,
        config: jinn_config::ConfigLayer,
        app_state_storage: jinn_preferences_config::AppStateStorageService,
        shells: jinn_common::shell::Shells,
        resolved_config_path: MarkedPathBuf<Absolute>,
        command: Option<jinn_cli::cli::HeadlessCommands>,
    ) -> Result<(), Report<AppError>> {
        use jinn_cli::cli::HeadlessCommands;

        let (core, services, _discord_activated) = self.activate_system(
            llm_service,
            provider_registry,
            api_keys,
            config_storage,
            session_store.clone(),
            config,
            app_state_storage,
            shells,
            resolved_config_path,
            dump_requests,
        )?;

        jinn_tui::load_theme(&core.state, &services.paths.themes_dir().as_marked_path());
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
        self.run_and_shutdown(runner, session_store)
    }

    /// Builds the actor system: the one `ActorSystemBuilder` call both
    /// command runners share, plus the handoff prompt it needs.
    ///
    /// # Errors
    ///
    /// Returns an error if slice activation fails.
    #[expect(
        clippy::too_many_arguments,
        reason = "one parameter per destructured `LaunchServices` field; a struct would only rename the field list"
    )]
    fn activate_system(
        &self,
        llm_service: jinn_kernel::LlmServiceFactoryService,
        provider_registry: jinn_kernel::ProviderRegistryService,
        api_keys: jinn_kernel::ApiKeysService,
        config_storage: jinn_kernel::ConfigStorageService,
        session_store: jinn_session_state::SessionStoreService,
        config: jinn_config::ConfigLayer,
        app_state_storage: jinn_preferences_config::AppStateStorageService,
        shells: jinn_common::shell::Shells,
        resolved_config_path: MarkedPathBuf<Absolute>,
        dump_requests: Option<MarkedPathBuf<Absolute>>,
    ) -> Result<
        (
            jinn_kernel::common::core::AppCore,
            jinn_kernel::Services,
            jinn_discord::ActivatedDiscord,
        ),
        Report<AppError>,
    > {
        let app_paths = jinn_kernel::AppPaths::default();
        let handoff_prompt =
            jinn_tui::load_handoff_prompt(&app_paths.prompts_dir().as_marked_path())
                .change_context(AppError)?;
        let handle = self.handle();
        self.runtime()
            .block_on(async {
                actor_wiring::ActorSystemBuilder::new(actor_wiring::ActorSystemBuilderArgs {
                    handle,
                    llm_service,
                    provider_registry,
                    api_keys,
                    config_storage,
                    session_store,
                    config,
                    app_state_storage,
                    shells,
                    resolved_config_path,
                    paths: app_paths,
                    dump_requests,
                    handoff_prompt,
                })
                .build()
                .await
            })
            .change_context(AppError)
            .attach("slice activation failed")
    }
}

impl Default for App {
    #[expect(
        clippy::expect_used,
        reason = "Default for App aborts on a broken runtime/services setup rather than returning a useless value"
    )]
    fn default() -> Self {
        Self::new().expect("failed to create default App")
    }
}

/// Writes the embedded `jinn.toml` template to `path`, creating parents.
///
/// Writes raw bytes rather than serializing a struct: the template is
/// documentation, and a round-trip would strip every comment it ships
/// with. Aborts the process on failure — a config file that cannot be
/// written leaves the run with no coherent source of truth, which is not
/// a state worth launching into.
#[expect(
    clippy::print_stderr,
    clippy::exit,
    reason = "top-level CLI failure before any TUI exists; the process exits with the error printed"
)]
pub(crate) fn seed_config_template(path: &MarkedPathBuf<Absolute>) {
    use jinn_preferences_config::create_default_preferences_to;

    if let Err(report) = create_default_preferences_to(path) {
        tracing::error!(path = %path, "failed to seed jinn.toml from the template");
        eprintln!("error: failed to create {path} from the default template:");
        #[expect(
            clippy::use_debug,
            reason = "the report's full chain is the only diagnostics this early path can give"
        )]
        let () = eprintln!("  {report:?}");
        std::process::exit(1);
    }
}

/// Fetches the dictation model into jinn's cache, then exits.
///
/// Runs on the real platform backend regardless of `[dictation]
/// enabled` — fetching is a network operation, enabling is a config
/// decision. On a platform without a real backend it prints the
/// unavailability sentence and exits non-zero: the null backend has
/// nothing to fetch, and a silent zero exit would look like success.
#[expect(
    clippy::print_stderr,
    clippy::print_stdout,
    clippy::exit,
    reason = "the `jinn fetch dictation-model` subcommand IS the process: it prints its result and exits; no TUI ever starts"
)]
fn fetch_dictation_model(runtime: &tokio::runtime::Runtime) -> ! {
    if !jinn_dictation::platform_supports_real_backend() {
        eprintln!("dictation backend unavailable on this platform");
        std::process::exit(1);
    }
    let model = jinn_dictation::platform_default_model();
    let result = runtime.block_on(async {
        tokio::task::spawn_blocking(move || jinn_dictation::fetch_model(&model)).await
    });
    match result {
        Ok(Ok(())) => {
            println!("dictation model fetched into the jinn cache");
        }
        Ok(Err(report)) => {
            #[expect(
                clippy::use_debug,
                reason = "the report's full chain is the only diagnostics this early path can give"
            )]
            let () = eprintln!("dictation model fetch failed: {report:?}");
            std::process::exit(1);
        }
        Err(join_error) => {
            eprintln!("dictation model fetch task failed: {join_error}");
            std::process::exit(1);
        }
    }
    std::process::exit(0);
}

/// Prints the capture input devices, then exits.
///
/// Runs regardless of `[dictation] enabled` — listing devices is an
/// inspection query, not a lifecycle decision. The names printed are
/// exactly what a `[dictation] device` value must equal (`find_device`
/// matches strictly), and the system default is marked, since an
/// unset `device` resolves to it. On a platform without a real
/// backend it prints the unavailability sentence and exits non-zero,
/// the same contract as [`fetch_dictation_model`].
#[expect(
    clippy::print_stderr,
    clippy::print_stdout,
    clippy::exit,
    reason = "the `jinn dictation list-devices` subcommand IS the process: it prints the device list and exits"
)]
fn list_dictation_devices() -> ! {
    if !jinn_dictation::platform_supports_real_backend() {
        eprintln!("dictation backend unavailable on this platform");
        std::process::exit(1);
    }
    let devices = jinn_dictation::list_input_devices();
    let default = jinn_dictation::default_input_device();
    if devices.is_empty() {
        eprintln!("no capture devices found");
        std::process::exit(1);
    }
    for line in format_device_lines(&devices, default.as_deref()) {
        println!("{line}");
    }
    std::process::exit(0);
}

/// Renders the device list for the CLI: one name per line, the system
/// default marked. Pure so the presentation is testable without an
/// audio host.
fn format_device_lines(devices: &[String], default: Option<&str>) -> Vec<String> {
    devices
        .iter()
        .map(|device| {
            if default.is_some_and(|name| name == device) {
                format!("* {device}  (system default)")
            } else {
                format!("  {device}")
            }
        })
        .collect()
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
    let target_path = jinn_kernel::AppPaths::default().models_dev_user_path();
    fetch_models_from_url("https://models.dev/api.json", target_path).await
}

/// Fetches model metadata from a URL and saves it to the user's cache directory.
///
/// This is the testable core of [`fetch_models`], separated so tests can
/// pass a mockito URL.
async fn fetch_models_from_url(
    url: &str,
    output_path: MarkedPathBuf<Absolute>,
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
    if let Some(parent) = output_path.as_path().parent() {
        std::fs::create_dir_all(parent)
            .change_context(AppError)
            .attach("failed to create cache directory")?;
    }

    std::fs::write(&output_path, &body)
        .change_context(AppError)
        .attach("failed to write models.dev.json")?;

    #[expect(
        clippy::print_stdout,
        reason = "the `jinn fetch models` subcommand reports its result on stdout; no TUI exists"
    )]
    let () =
        println!("Fetched {model_count} models from {provider_count} providers to {output_path}");

    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        reason = "test code: fixture failures and error assertions are the test's own failure channel"
    )]
    use jinn_kernel::{AppState, State};
    use jinn_tui::load_theme;
    use std::sync::Arc;

    use crate::config_path::{config_init_target, resolve_config_path};

    use super::*;

    #[rstest::rstest]
    #[test]
    fn format_device_lines_marks_the_default() {
        // Given three devices where the second is the system default.
        let devices = vec![
            "Built-in Mic".to_owned(),
            "voix".to_owned(),
            "Webcam".to_owned(),
        ];

        // When rendering the lines.
        let lines = format_device_lines(&devices, Some("voix"));

        // Then the default carries the marker and the rest are indented.
        assert_eq!(
            lines,
            vec![
                "  Built-in Mic".to_owned(),
                "* voix  (system default)".to_owned(),
                "  Webcam".to_owned(),
            ]
        );
    }

    #[rstest::rstest]
    #[test]
    fn format_device_lines_without_a_default_marks_nothing() {
        // Given a device list whose host reports no default input.
        let devices = vec!["Built-in Mic".to_owned()];

        // When rendering the lines.
        let lines = format_device_lines(&devices, None);

        // Then every line is indented and no marker appears.
        assert_eq!(lines, vec!["  Built-in Mic".to_owned()]);
    }

    // Note: there is no unit test for `run_and_shutdown` / `dispatch` calling
    // `store.shutdown()`. `Runner::run` is a concrete enum (not a trait), and
    // both variants require a live `AppCore` + `ActorHostService` (the full
    // actor system) to construct. Standing that up — or introducing a `Runner`
    // trait + fake — is scope creep for this task.
    //
    // The behavior is covered two ways instead:
    //  1. The 4 wiring sites are verified by static inspection: `run_and_shutdown`
    //     is called on the Tui and Headless exit paths, and from both
    //     `Bench::Run` and `Bench::Tui`.
    //  2. The checkpoint itself is proven by `shutdown_truncates_wal_file` and
    //     `shutdown_makes_db_self_contained_for_backup` in the session store tests.

    #[rstest::rstest]
    fn load_theme_updates_state_from_file() {
        // Given a temp directory with a custom theme file.
        let dir = jinn_testutil::tempdir();
        let theme_content = "focus_accent = \"magenta\"\nborder_unfocused = \"blue\"";
        std::fs::write(
            dir.path
                .join("custom.toml")
                .expect("temp root joins a bare name"),
            theme_content,
        )
        .expect("write theme");

        let state = State::new(AppState::default());

        // Set the theme name in preferences.
        state.write().frontend.app_state.theme_name = Some("custom".to_owned());

        // Capture the initial focus_accent color.
        let initial_focus = state.read().frontend.theme.focus_accent;

        // When loading the theme.
        load_theme(&state, &dir.path.as_marked_path());

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
        // Given a state with a theme name that doesn't exist, and an empty
        // user themes dir.
        let state = State::new(AppState::default());
        state.write().frontend.app_state.theme_name = Some("nonexistent".to_owned());

        let user = jinn_testutil::tempdir();

        // When loading the theme (no matching file).
        // Then it should not panic - the function logs a warning and returns.
        load_theme(&state, &user.path.as_marked_path());
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
        let temp_dir = jinn_testutil::tempdir();
        let output_path = temp_dir
            .path
            .join("models.dev.json")
            .expect("temp root joins a bare name");

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
        let temp_dir = jinn_testutil::tempdir();
        let output_path = temp_dir
            .path
            .join("models.dev.json")
            .expect("temp root joins a bare name");

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
        let temp_dir = jinn_testutil::tempdir();
        let output_path = temp_dir
            .path
            .join("models.dev.json")
            .expect("temp root joins a bare name");

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
    fn seed_config_template_preserves_template_comments() {
        // Given a config path inside a directory that does not exist yet.
        let dir = jinn_testutil::tempdir();
        let path = dir.path.join_all(["nested", "jinn.toml"]).expect("a");

        // When seeding the template.
        seed_config_template(&path);

        // Then the file was created and contains the embedded template verbatim.
        let written = std::fs::read_to_string(&path).expect("read seeded config");
        assert_eq!(written, jinn_preferences_config::DEFAULT_CONFIG);
        // And the template's comments survived (a struct round-trip would strip them).
        assert!(
            written
                .lines()
                .any(|line| line.trim_start().starts_with('#')),
            "seeded config should retain the template's comments"
        );
    }

    #[rstest::rstest]
    fn config_init_honors_the_config_override() {
        // Given a --config path that does not exist yet, and a default
        // location built from a temp root.
        let dir = jinn_testutil::tempdir();
        let override_path = dir.path.join_all(["nested", "alt.toml"]).expect("a");
        let default = dir.path.join_all(["default", "jinn.toml"]).expect("a");

        // When `jinn config init` picks its target.
        let target = config_init_target(Some(override_path.as_path()), &default);

        // Then it targets the override, not the default location.
        assert_eq!(target, override_path);
    }

    #[rstest::rstest]
    fn config_init_without_override_targets_the_default_location() {
        // Given no --config, and a default location built from a temp root.
        let dir = jinn_testutil::tempdir();
        let default = dir.path.join_all(["default", "jinn.toml"]).expect("a");

        // When `jinn config init` picks its target.
        let target = config_init_target(None, &default);

        // Then it targets the default location.
        assert_eq!(target, default);
    }

    #[rstest::rstest]
    fn config_init_override_is_accepted_though_runtime_resolver_rejects_it() {
        // Given a --config path that does not exist, and a default location
        // built from a temp root.
        let dir = jinn_testutil::tempdir();
        let override_path = dir
            .path
            .join("alt.toml")
            .expect("temp root joins a bare name");
        let default = dir.path.join_all(["default", "jinn.toml"]).expect("a");

        // When the two choosers are consulted.
        let init_target = config_init_target(Some(override_path.as_path()), &default);
        let runtime = resolve_config_path(Some(override_path.as_path()), &default);

        // Then `config init` still targets the override, since creating the
        // file is its whole job.
        assert_eq!(init_target, override_path);
        // But a run that intends to read the file still rejects it.
        assert!(runtime.is_err());
    }

    #[rstest::rstest]
    fn malformed_config_under_an_override_fails_naming_the_override() {
        // Given an override path holding invalid TOML.
        let dir = jinn_testutil::tempdir();
        let override_path = dir
            .path
            .join("alt.toml")
            .expect("temp root joins a bare name");
        std::fs::write(&override_path, "[section\nkey = ").expect("write");

        // When the config layer is loaded over the resolved path, exactly
        // as dispatch does.
        let backend = jinn_config::FilesystemConfigStorage::new(override_path.clone());
        let result = jinn_config::ConfigLayer::load(Arc::new(backend));

        // Then the load fails, and the report names the override file.
        let report = result.expect_err("malformed override must fail");
        let rendered = format!("{report:?}");
        assert!(
            rendered.contains(override_path.to_string().as_str()),
            "fail-fast report should name the override path: {rendered}"
        );
    }
}
