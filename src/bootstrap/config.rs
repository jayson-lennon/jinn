//! Config reading and service construction for a launch.
//!
//! Everything a run needs before any actor is spawned: the early-return
//! CLI subcommands, the resolved config path, the configuration layer and
//! its validation, the session store, and the services built over them.
//!
//! The order here is behavioural. The recovery subcommands run *first* so
//! `jinn config init` still works when the file it would repair is
//! unreadable; the session store opens *after* they return, so a recovery
//! command never pays for a database open; and validation runs *before*
//! any wiring, so a malformed `jinn.toml` fails fast instead of booting an
//! app that reads defaults.

use std::sync::Arc;

use error_stack::Report;
use error_stack::ResultExt;

use jinn_kernel::ApiKeys;
use jinn_kernel::ApiKeysService;
use jinn_kernel::ConfigStorageService;
use jinn_kernel::FilesystemConfigStorage;
use jinn_kernel::LlmServiceFactoryService;
use jinn_kernel::NoProvidersAvailableFactory;
use jinn_kernel::ProviderRegistry;
use jinn_kernel::ProviderRegistryService;
use jinn_preferences_config::AppStateStorageService;
use jinn_preferences_config::FilesystemAppStateStorage;
use jinn_session_state::SessionStoreService;

use crate::app::AppError;
use crate::app::seed_config_template;
use crate::config_path::config_init_target;
use crate::config_path::resolve_config_path;
use marked_path::{Absolute, MarkedPathAccess, MarkedPathBuf};

/// The services a run is constructed from, before any actor exists.
///
/// Returned whole so the composition root receives a finished set rather
/// than assembling services piecemeal at the point of use.
pub struct LaunchServices {
    /// Backend over `providers.toml`.
    pub config_storage: ConfigStorageService,
    /// Resolved by the env-init actor at boot.
    pub api_keys: ApiKeysService,
    /// Populated by the provider-init actor at boot.
    pub provider_registry: ProviderRegistryService,
    /// The no-provider sentinel until the actors resolve the real one.
    pub llm_service: LlmServiceFactoryService,
    /// The session database.
    pub session_store: SessionStoreService,
    /// The pool behind `session_store`, for headless callers.
    pub session_pool: jinn_discord::backend::spawn::SessionPool,
    /// The validated configuration layer.
    pub config: jinn_config::ConfigLayer,
    /// Persisted app state, reloaded from disk.
    pub app_state_storage: AppStateStorageService,
    /// The shells jinn runs commands through, resolved at launch.
    pub shells: jinn_common::shell::Shells,
    /// The absolute path of the `jinn.toml` document this run reads and
    /// writes: the resolved `--config` override, or the default preferences
    /// path when the flag is absent. The config-watch slice watches this
    /// file; the layer already reads and writes it.
    pub resolved_config_path: MarkedPathBuf<Absolute>,
}

/// Outcome of the pre-wiring phase.
pub enum Prepared {
    /// A recovery subcommand already ran and the process should exit.
    Handled,
    /// A bundled MCP server should run over stdio with this database
    /// directory, then the process should exit. Carries the `--db` override
    /// so `dispatch` never re-reads the CLI.
    ServeBundled {
        /// Database directory override; `None` uses the jinn-managed default.
        #[expect(
            clippy::disallowed_types,
            reason = "argv transit: carries the raw --db flag from the parse to dispatch, where resolve_db_dir types it"
        )]
        db: Option<std::path::PathBuf>,
        /// Embedding-model override; `None` uses the catalog default.
        embed_model: Option<jinn_mcp_overgraph::embedder::EmbedderModel>,
        /// Warm the embedding model's cache and exit without serving.
        fetch_model: bool,
    },
    /// The run is configured; carry on to wire the actors.
    Ready(LaunchServices),
}

/// Reads config and builds services, or runs a recovery subcommand.
///
/// # Errors
///
/// Returns an error if a recovery subcommand fails, or if the session
/// store cannot be opened. A malformed or invalid `jinn.toml` is *not*
/// an error return: those exit the process with a message, because the
/// user has no actor to see a `Report` from yet.
pub fn prepare(
    cli: &jinn_cli::cli::Cli,
    runtime: &tokio::runtime::Runtime,
) -> Result<Prepared, Report<AppError>> {
    if let Some(handled) = run_recovery_subcommand(cli)? {
        return Ok(handled);
    }

    // The external-remote client intercepts before any config read or
    // session-store open: it is a short-lived HTTP caller that needs
    // only the registry file under the state dir, so a fresh machine
    // without a config can still list instances.
    if let Some(jinn_cli::cli::Commands::Remote { subcommand }) = &cli.command {
        crate::remote_cli::run(subcommand.clone(), runtime)
            .map_err(|report| report.change_context(AppError))?;
        return Ok(Prepared::Handled);
    }

    reconcile_bundled_resources();

    let (config_storage, config, app_state_storage, resolved_config_path) = read_config(cli)?;
    let shells = resolve_shells(&config);
    let (session_store, session_pool) = open_session_store(cli, runtime)?;

    Ok(Prepared::Ready(LaunchServices {
        config_storage,
        api_keys: ApiKeysService::new(ApiKeys::new()),
        provider_registry: ProviderRegistryService::new(
            ProviderRegistry::from_config(jinn_kernel::ProvidersConfig {
                providers: std::collections::BTreeMap::new(),
                aliases: vec![],
                default_provider: None,
                endpoint_defaults: vec![],
            })
            .change_context(AppError)?,
        ),
        llm_service: LlmServiceFactoryService::new(Arc::new(NoProvidersAvailableFactory)),
        session_store,
        session_pool,
        config,
        app_state_storage,
        shells,
        resolved_config_path,
    }))
}

/// Reconciles the bundled `res/` resources against the on-disk copies and
/// the state-dir manifest, healing drift from a binary update.
///
/// Runs once per launch, after the recovery-subcommand early return — so
/// `jinn install`, `jinn config`, and the bundled MCP server children never
/// reconcile as a side effect — and before any resource loader reads disk.
///
/// Never fatal: every problem is logged at INFO/WARN to `jinn.log` and the
/// launch proceeds. A read-only config dir must not brick the TUI. The
/// resource loaders stay pure-disk readers; this is the only writer.
fn reconcile_bundled_resources() {
    let app_paths = jinn_kernel::AppPaths::default();
    let destinations = jinn_install::destinations_from_paths(&app_paths);

    // The expected set is derived from the compile-time embed; a
    // derivation failure is a build-shape bug and skips the reconcile.
    let expected = match jinn_install::expected_catalogue() {
        Ok(expected) => expected,
        Err(report) => {
            tracing::warn!(%report, "bundled-resource catalogue derivation failed; skipping reconcile");
            return;
        }
    };

    let report = jinn_install::reconcile(
        &destinations,
        &expected,
        &app_paths.bundled_files_manifest_path(),
    );

    let counts = report.counts();
    for warning in &report.warnings {
        tracing::warn!(
            warning,
            "bundled-resource reconcile hit a non-fatal problem"
        );
    }
    for action in &report.actions {
        let path = action.path();
        match action {
            jinn_install::ReconcileAction::Created(_) => {
                tracing::info!(%path, "bundled resource created");
            }
            jinn_install::ReconcileAction::Overwritten(_) => {
                tracing::info!(%path, "stale bundled resource overwritten");
            }
            jinn_install::ReconcileAction::Adopted(_) => {
                tracing::info!(%path, "bundled resource adopted");
            }
            jinn_install::ReconcileAction::Reclaimed(_) => {
                tracing::info!(%path, "bundled resource reclaimed");
            }
            jinn_install::ReconcileAction::Unchanged(_)
            | jinn_install::ReconcileAction::KeptUserEdited(_)
            | jinn_install::ReconcileAction::KeptOrphan(_) => {}
            jinn_install::ReconcileAction::DeletedObsolete(_) => {
                tracing::info!(%path, "obsolete bundled resource deleted");
            }
        }
    }
    tracing::info!(
        created = counts.created,
        overwritten_stale = counts.overwritten_stale,
        adopted = counts.adopted,
        unchanged = counts.unchanged,
        kept_user_edited = counts.kept_user_edited,
        deleted_obsolete = counts.deleted_obsolete,
        kept_orphan = counts.kept_orphan,
        "bundled-resource reconcile complete"
    );
}

/// Resolves the shells jinn runs commands through, from the `[shell]`
/// section of `jinn.toml`.
///
/// A configured path that names no file is a typo, and it stops the
/// launch: the user configured that executable deliberately, and falling
/// through to auto-detection would silently run something else — on
/// Windows, potentially the WSL launcher. A shell that is simply not
/// installed is a fact about the machine, not a configuration error, so
/// it is reported at the point of use instead.
///
/// The environment is probed here, at launch, and never again: the
/// resolved value is carried on [`jinn_kernel::Services`] from here on.
fn resolve_shells(config: &jinn_config::ConfigLayer) -> jinn_common::shell::Shells {
    // A malformed `[shell]` section is a broken file, not a broken
    // machine: it stops the launch the same way a broken `jinn.toml`
    // does, naming the section.
    let shell_config = match config.get::<jinn_preferences_config::schemas::ShellConfig>() {
        Ok(shell_config) => shell_config,
        Err(error) => {
            eprintln!("error: [shell] in jinn.toml is malformed: {error}");
            std::process::exit(1);
        }
    };
    let shells = jinn_common::shell::Shells::resolve(
        shell_config.bash_path.as_deref(),
        shell_config.powershell_path.as_deref(),
    );
    for error in [shells.bash().err(), shells.powershell().err()]
        .into_iter()
        .flatten()
    {
        if error.is_configuration_error() {
            eprintln!("error: {error}");
            std::process::exit(1);
        }
    }
    shells
}

/// The subcommands that must run before anything is parsed or opened.
///
/// `jinn config init` is the recovery tool for a missing or broken config,
/// so it must not be guarded by the parsing that auto-creates the file on
/// first run — and it needs no session store.
fn run_recovery_subcommand(cli: &jinn_cli::cli::Cli) -> Result<Option<Prepared>, Report<AppError>> {
    use jinn_cli::cli::Commands;

    if let Some(Commands::Config { subcommand }) = &cli.command {
        use jinn_cli::cli::ConfigCommands;
        use jinn_preferences_config::{
            InitOutcome, StreamRulesSeedOutcome, init_default_config_to, init_stream_rules_to,
            preferences_path,
        };

        match subcommand {
            ConfigCommands::Init { force } => {
                // Honor --config here: the user naming a path is asking
                // for the file to land there. Deliberately skips the
                // existence check the runtime resolver applies, since
                // creating the file is what this command is for.
                let path = config_init_target(cli.config.as_deref(), &preferences_path());
                let force = *force;
                match init_default_config_to(&path, force) {
                    Ok(InitOutcome::Created) => {
                        println!("Created {path}");
                    }
                    Ok(InitOutcome::Overwritten) => {
                        println!("Overwrote {path}");
                    }
                    Err(report) => {
                        eprintln!("{report:?}");
                        return Err(report.change_context(AppError));
                    }
                }

                // The stream-rules file lives beside `jinn.toml`, wherever
                // this command just put that document. `--force` overwrites
                // it too — that is the supported path for pulling in an
                // updated shipped set. Without `--force` an existing file is
                // left alone and reported as skipped, not failed: the
                // command's primary target (`jinn.toml`) already succeeded.
                let view = path.as_marked_path();
                let rules_path = {
                    // `parent()` is `None` only for the filesystem root,
                    // which is then its own parent.
                    let base = view.parent().unwrap_or(view);
                    base.join(jinn_preferences_config::STREAM_RULES_FILE_NAME)
                        .expect("absolute parent joins a bare filename")
                };
                match init_stream_rules_to(&rules_path, force) {
                    Ok(StreamRulesSeedOutcome::Created(p)) => {
                        println!("Created {p}");
                    }
                    Ok(StreamRulesSeedOutcome::Overwritten(p)) => {
                        println!("Overwrote {p}");
                    }
                    Ok(StreamRulesSeedOutcome::Untouched(p)) => {
                        println!("Already present, skipped {p}");
                    }
                    Err(report) => {
                        eprintln!("{report:?}");
                        return Err(report.change_context(AppError));
                    }
                }
                return Ok(Some(Prepared::Handled));
            }
            ConfigCommands::Providers { force } => {
                use jinn_kernel::{InitProvidersOutcome, config_path, init_default_providers_to};

                let path = config_path();
                let force = *force;
                match init_default_providers_to(&path, force) {
                    Ok(InitProvidersOutcome::Created) => {
                        println!("Created {path}");
                    }
                    Ok(InitProvidersOutcome::Overwritten) => {
                        println!("Overwrote {path}");
                    }
                    Err(report) => {
                        eprintln!("{report:?}");
                        return Err(report.change_context(AppError));
                    }
                }
                return Ok(Some(Prepared::Handled));
            }
        }
    }

    // `install` seeds the three config files, write-once, and — with
    // `--builtins` — force-restores the bundled resources. Like `config`,
    // it must run before any actor wiring — and it needs no preferences/DB,
    // so it dispatches before the session store is opened.
    if let Some(Commands::Install { builtins }) = &cli.command {
        use jinn_install::{
            SeedOutcome, destinations_from_paths, expected_catalogue, restore_builtins,
            seed_configs_to,
        };
        use jinn_kernel::AppPaths;
        use jinn_preferences_config::StreamRulesSeedOutcome;

        let app_paths = AppPaths::default();
        let destinations = destinations_from_paths(&app_paths);

        // `--builtins` is the explicit reclaim: overwrite every bundled
        // resource from the embed (including user edits), then exit. It
        // touches no config file.
        if *builtins {
            let expected = expected_catalogue().map_err(|report| {
                eprintln!("error: failed to enumerate bundled resources:");
                eprintln!("  {report:?}");
                report.change_context(AppError)
            })?;
            match restore_builtins(
                &destinations,
                &expected,
                &app_paths.bundled_files_manifest_path(),
            ) {
                Ok(report) => {
                    for action in &report.actions {
                        println!("Restored {}", action.path());
                    }
                    for warning in &report.warnings {
                        eprintln!("warning: {warning}");
                    }
                }
                Err(report) => {
                    eprintln!("error: failed to restore bundled resources:");
                    eprintln!("  {report:?}");
                    return Err(report.change_context(AppError));
                }
            }
            return Ok(Some(Prepared::Handled));
        }

        // Plain install: config files only, each created only when missing.
        // An existing file is never read or modified — never an error.
        let prefs_path = app_paths.preferences_path();
        let providers_path = jinn_kernel::config_path();
        match seed_configs_to(&prefs_path, &providers_path) {
            Ok(report) => {
                for (name, outcome) in [
                    ("jinn.toml", &report.jinn_toml),
                    ("providers.toml", &report.providers_toml),
                ] {
                    match outcome {
                        SeedOutcome::Created(path) => println!("Created {path}"),
                        SeedOutcome::Untouched(path) => {
                            println!("Already present, skipped {path} ({name})");
                        }
                    }
                }
                match &report.stream_rules {
                    StreamRulesSeedOutcome::Created(path) => {
                        println!("Created {path}");
                    }
                    // Install never modifies an existing rules file, so
                    // Untouched is the only other reachable variant here.
                    StreamRulesSeedOutcome::Untouched(path)
                    | StreamRulesSeedOutcome::Overwritten(path) => {
                        println!("Already present, skipped {path}");
                    }
                }
                return Ok(Some(Prepared::Handled));
            }
            Err(report) => {
                eprintln!("error: failed to install defaults:");
                eprintln!("  {report:?}");
                return Err(report.change_context(AppError));
            }
        }
    }

    // The bundled MCP servers (`jinn mcp overgraph`) are child-process
    // programs: they speak stdio MCP and touch nothing from the parent's
    // environment — no jinn.toml, no providers, no sessions.db. They
    // intercept here so a server spawn never opens the session store or
    // mutates the user's config as a side effect; `dispatch` runs the
    // server from the returned variant.
    if let Some(Commands::Mcp {
        subcommand:
            jinn_cli::cli::McpCommands::Overgraph {
                db,
                embed_model,
                fetch_model,
            },
    }) = &cli.command
    {
        // A malformed --embed-model is a CLI-level usage error: it is
        // rendered here (before any transport exists) and exits the process,
        // matching how a bad flag itself would fail. The server never starts.
        let embed_model = match embed_model {
            Some(name) => match jinn_mcp_overgraph::embedder::EmbedderModel::from_arg(name) {
                Some(model) => Some(model),
                None => {
                    #[expect(
                        clippy::print_stderr,
                        reason = "fatal CLI usage failure before any transport starts"
                    )]
                    let () = {
                        eprintln!(
                            "error: unknown embedding model `{name}`; \
                                 valid models: bge-small-en-v1.5"
                        );
                    };
                    return Err(
                        Report::new(AppError).attach(format!("unknown --embed-model `{name}`"))
                    );
                }
            },
            None => None,
        };
        return Ok(Some(Prepared::ServeBundled {
            db: db.clone(),
            embed_model,
            fetch_model: *fetch_model,
        }));
    }

    Ok(None)
}

/// Checks that `providers.toml` loads and parses, producing a fail-fast report.
///
/// A missing file is not an error here — the loader auto-creates the default
/// template on first run. Only a load or parse failure produces an error, with
/// the config path and the underlying TOML detail attached to the report.
fn providers_load_error_report(storage: &ConfigStorageService) -> Result<(), Report<AppError>> {
    if let Err(report) = storage.load() {
        let path = jinn_kernel::config_path();
        return Err(report
            .change_context(AppError)
            .attach(format!("failed to load providers config at {path}")));
    }
    Ok(())
}

/// Resolves the config path, loads the layer, validates it, and reloads
/// app state.
type ConfigTriple = (
    ConfigStorageService,
    jinn_config::ConfigLayer,
    AppStateStorageService,
    MarkedPathBuf<Absolute>,
);

fn read_config(cli: &jinn_cli::cli::Cli) -> Result<ConfigTriple, Report<AppError>> {
    let config_storage =
        ConfigStorageService::new(Arc::new(FilesystemConfigStorage::default_path()));

    // Resolve which jinn.toml this run reads and writes, and seed the
    // template when the default location is still missing.
    //
    // This runs AFTER the recovery subcommands above so neither gets
    // pre-seeded ahead of itself, and BEFORE the layer load so the seeded
    // file is what the layer reads.
    let resolved_config = resolve_config_path(
        cli.config.as_deref(),
        &jinn_preferences_config::preferences_path(),
    );
    let config_path = match resolved_config {
        Ok(resolved) => {
            if resolved.seed_template {
                seed_config_template(&resolved.path);
            }
            resolved.path
        }
        Err(report) => {
            eprintln!("error: failed to resolve the configuration path:");
            eprintln!("  {report:?}");
            std::process::exit(1);
        }
    };

    // The layer is the only reader of this document; there is no separate
    // aggregate struct to parse alongside it. The storage is built over
    // the RESOLVED path, so `--config` redirects reads and writes alike.
    let config = {
        let backend = jinn_config::FilesystemConfigStorage::new(config_path.clone());
        match jinn_config::ConfigLayer::load(Arc::new(backend)) {
            Ok(layer) => layer,
            Err(report) => {
                tracing::error!(path = %config_path, "failed to load the jinn.toml configuration layer");
                eprintln!("error: failed to parse {config_path}:");
                eprintln!("  {report:?}");
                std::process::exit(1);
            }
        }
    };

    // Fail-fast on a malformed section before any actor wiring runs.
    // `validate` only walks sections registered on the layer, so the
    // roster goes in first — an unregistered section is never checked
    // and a malformed table would boot to a running app reading
    // defaults. Registration must precede the check, and both must
    // precede the composition root's boot.
    jinn_preferences_config::register_all_sections(&config);

    if let Err(error) = config.validate() {
        tracing::error!(%error, "jinn.toml section failed validation");
        eprintln!("error: {error}");
        std::process::exit(1);
    }

    // Fail-fast on a malformed providers.toml before any actor wiring
    // runs. `jinn config providers` has already dispatched, so it
    // remains usable as the recovery tool for a broken file.
    if let Err(report) = providers_load_error_report(&config_storage) {
        tracing::error!("failed to load providers config");
        eprintln!("error: failed to load providers config:");
        eprintln!("  {report:?}");
        std::process::exit(1);
    }

    let app_state_storage = {
        let backend =
            FilesystemAppStateStorage::new(jinn_kernel::AppPaths::default().state_file_path());
        let svc = AppStateStorageService::new(Arc::new(backend));
        if let Err(report) = svc.reload() {
            tracing::error!("failed to load app state");
            eprintln!("error: failed to load app state:");
            eprintln!("  {report:?}");
            std::process::exit(1);
        }
        svc
    };

    Ok((config_storage, config, app_state_storage, config_path))
}

/// Resolves a `--db-path` CLI value into a typed absolute path.
///
/// A relative argument is the user's cwd-relative shorthand: it resolves
/// against the process cwd, the same rule every relative CLI path in jinn
/// follows, and a leading `~` expands against the home directory. The
/// resolution rules live in [`crate::cli_paths`], shared with `--log-file`
/// and `--dump-requests`.
///
/// # Errors
///
/// Returns an error when the process cwd is unavailable (needed to resolve
/// a relative argument) or when the value cannot be an absolute path.
fn resolve_db_path(path: &std::path::Path) -> Result<MarkedPathBuf<Absolute>, Report<AppError>> {
    resolve_db_path_with(path, jinn_kernel::AppPaths::default().home_dir())
}

/// [`resolve_db_path`] with the home directory injected, so tests expand a
/// tilde against a temp home instead of the process home.
fn resolve_db_path_with(
    path: &std::path::Path,
    home: &MarkedPathBuf<Absolute>,
) -> Result<MarkedPathBuf<Absolute>, Report<AppError>> {
    crate::cli_paths::resolve_cli_path("--db-path", path, home).change_context(AppError)
}

/// Opens the session database, honouring `--db-path`.
fn open_session_store(
    cli: &jinn_cli::cli::Cli,
    runtime: &tokio::runtime::Runtime,
) -> Result<
    (
        SessionStoreService,
        jinn_discord::backend::spawn::SessionPool,
    ),
    Report<AppError>,
> {
    let typed_db_path = cli
        .db_path_opt()
        .map(|path| resolve_db_path(path))
        .transpose()?;
    let store = runtime.block_on(async {
        match typed_db_path.as_ref() {
            Some(typed) => {
                jinn_session_store::sqlite::SqliteSessionStore::open_or_create(typed).await
            }
            None => jinn_session_store::sqlite::SqliteSessionStore::new().await,
        }
    });
    let store = store.change_context(AppError)?;
    let pool = store.pool().clone();
    Ok((SessionStoreService::new(Arc::new(store)), pool))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "test code")]

    use jinn_kernel::ConfigStorageService;
    use jinn_kernel::FilesystemConfigStorage;
    use marked_path::{Absolute, MarkedPathAccess, MarkedPathBuf};
    use std::sync::Arc;

    use super::resolve_db_path;
    use super::resolve_db_path_with;

    use super::providers_load_error_report;
    #[rstest::rstest]
    fn providers_load_error_report_fails_with_parse_detail_on_malformed_file() {
        // Given a config storage backed by a malformed providers.toml.
        let dir = jinn_testutil::tempdir();
        let path = dir
            .path
            .join("providers.toml")
            .expect("temp root joins a bare name");
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
    fn resolve_db_path_resolves_a_relative_argument_against_the_process_cwd() {
        // Given a relative db path argument.
        let arg = std::path::Path::new("jinn-sessions.db");

        // When resolving it.
        let resolved = resolve_db_path(arg).expect("relative arg resolves");

        // Then it lands directly under the process cwd.
        let expected =
            MarkedPathBuf::<Absolute>::new(std::env::current_dir().expect("process cwd"))
                .expect("process cwd is absolute")
                .join("jinn-sessions.db")
                .expect("cwd joins a bare filename");
        assert_eq!(resolved, expected);
    }

    #[rstest::rstest]
    fn resolve_db_path_keeps_an_absolute_argument_verbatim() {
        // Given an absolute db path argument.
        let dir = jinn_testutil::tempdir();
        let arg = dir
            .path
            .join("sessions.db")
            .expect("temp root joins a bare name");

        // When resolving it.
        let resolved = resolve_db_path(arg.as_path()).expect("absolute arg resolves");

        // Then it round-trips unchanged.
        assert_eq!(resolved, arg);
    }

    #[rstest::rstest]
    fn resolve_db_path_expands_a_tilde_argument_against_the_injected_home() {
        // Given a `~/`-prefixed db path argument and an injected temp home.
        let dir = jinn_testutil::tempdir();
        let home = dir.path.join("home").expect("temp root joins a bare name");
        let arg = std::path::Path::new("~/sessions.db");

        // When resolving it.
        let resolved = resolve_db_path_with(arg, &home).expect("tilde arg resolves");

        // Then it lands under the injected home, not the process home.
        let expected = home.join("sessions.db").expect("home joins a bare name");
        assert_eq!(resolved, expected);
    }

    #[rstest::rstest]
    fn providers_load_error_report_ok_when_file_missing() {
        // Given a config storage backed by a directory with no providers.toml.
        let dir = jinn_testutil::tempdir();
        let path = dir
            .path
            .join("providers.toml")
            .expect("temp root joins a bare name");
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
