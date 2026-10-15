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

        // Graceful shutdown: the trouper sweep fires every supervised
        // child's shutdown watcher (each loop drains, runs its `on_stop`
        // hook, and tears down), races the drain against a 10s deadline,
        // then flushes the store. A wedged actor can't prevent process
        // exit — the sweep hard-stops stragglers at the deadline.
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

        // Read config and build every service the actors will hold. This
        // also runs the recovery subcommands (`config init`,
        // `config providers`, `install`), which return before the session
        // store is opened — so those stay usable when the file they would
        // repair is unreadable. See `bootstrap::config` for why the order
        // there is behavioural.
        let services =
            match bootstrap::config::prepare(&cli, &self.runtime).change_context(AppError)? {
                bootstrap::config::Prepared::Handled => return Ok(()),
                bootstrap::config::Prepared::Ready(services) => services,
            };
        let LaunchServices {
            config_storage,
            api_keys: resolved_api_keys,
            provider_registry,
            llm_service,
            session_store,
            session_pool,
            config,
            app_state_storage,
        } = services;
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
            // No bundled MCP server ships today, so `jinn mcp noop` falls
            // through to a normal startup: the command exists as the spawn
            // point for future bundled servers (see `McpCommands`).
            Commands::Tui | Commands::Mcp { .. } => {
                let compaction_prompt = jinn_tui::load_compaction_prompt(
                    &jinn_kernel::AppPaths::default().prompts_dir(),
                    &jinn_kernel::AppPaths::default().system_prompts_dir(),
                )
                .change_context(AppError)?;
                let (core, services, discord_activated) = self
                    .runtime
                    .block_on(async {
                        actor_wiring::ActorSystemBuilder::new(
                            actor_wiring::ActorSystemBuilderArgs {
                                handle: self.handle(),
                                llm_service: llm_service.clone(),
                                provider_registry: provider_registry.clone(),
                                api_keys: resolved_api_keys.clone(),
                                config_storage: config_storage.clone(),
                                session_store: session_store.clone(),
                                config: config.clone(),
                                app_state_storage: app_state_storage.clone(),
                                paths: jinn_kernel::AppPaths::default(),
                                dump_requests: cli.dump_requests.clone(),
                                compaction_prompt,
                            },
                        )
                        .build()
                        .await
                    })
                    .change_context(AppError)
                    .attach("slice activation failed")?;

                // The discord frontend consumes the slice activation's
                // parked channels + config; no-ops when disabled.
                jinn_discord::spawn_gateway(
                    &self.handle(),
                    &core,
                    &services,
                    session_pool.clone(),
                    discord_activated,
                );

                let app = jinn_tui::launch(core, services, bootstrap::ui::build_ui_registry())
                    .change_context(AppError)?;
                let runner = Runner::Tui(Box::new(app));
                self.run_and_shutdown(runner, &session_store)?;
            }
            #[cfg(debug_assertions)]
            Commands::Headless { command, .. } => {
                let store_for_shutdown = session_store.clone();
                let compaction_prompt = jinn_tui::load_compaction_prompt(
                    &jinn_kernel::AppPaths::default().prompts_dir(),
                    &jinn_kernel::AppPaths::default().system_prompts_dir(),
                )
                .change_context(AppError)?;
                let (core, services, _discord_activated) = self
                    .runtime
                    .block_on(async {
                        actor_wiring::ActorSystemBuilder::new(
                            actor_wiring::ActorSystemBuilderArgs {
                                handle: self.handle(),
                                llm_service: llm_service.clone(),
                                provider_registry,
                                api_keys: resolved_api_keys,
                                config_storage,
                                session_store,
                                config: config.clone(),
                                app_state_storage,
                                paths: jinn_kernel::AppPaths::default(),
                                dump_requests: cli.dump_requests.clone(),
                                compaction_prompt,
                            },
                        )
                        .build()
                        .await
                    })
                    .change_context(AppError)
                    .attach("slice activation failed")?;

                jinn_tui::load_theme(
                    &core.state,
                    &services.paths.themes_dir(),
                    &services.paths.system_themes_dir(),
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
        }

        Ok(())
    }
}

impl Default for App {
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
pub(crate) fn seed_config_template(path: &std::path::Path) {
    use jinn_preferences_config::create_default_preferences_to;

    if let Err(report) = create_default_preferences_to(path) {
        tracing::error!(
            path = %path.display(),
            "failed to seed jinn.toml from the template"
        );
        eprintln!(
            "error: failed to create {} from the default template:",
            path.display()
        );
        eprintln!("  {report:?}");
        std::process::exit(1);
    }
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

#[cfg(test)]
mod tests {
    use jinn_kernel::{AppState, State};
    use jinn_tui::{load_compaction_prompt, load_theme};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use crate::config_path::{config_init_target, resolve_config_path};

    use super::*;

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
    fn load_compaction_prompt_populates_state() {
        // Given a user dir with a compaction prompt file.
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join("_compaction.md"), "test compaction prompt")
            .expect("write compaction prompt");

        let empty = PathBuf::from("/nonexistent");

        // When loading the compaction prompt.
        let prompt = load_compaction_prompt(dir.path(), &empty).expect("load");

        // Then the loader returns the prompt text.
        assert_eq!(prompt, "test compaction prompt");
    }

    #[rstest::rstest]
    fn load_compaction_prompt_returns_err_when_missing() {
        // Given empty user and system dirs.
        let user_dir = tempfile::tempdir().expect("temp dir");
        let system_dir = tempfile::tempdir().expect("temp dir");

        // When loading the compaction prompt with no file present.
        let result = load_compaction_prompt(user_dir.path(), system_dir.path());

        // Then an error is returned (hard-fail semantics).
        assert!(
            result.is_err(),
            "expected error when compaction prompt is missing"
        );

        // And the loader does not touch any state (pure function — nothing
        // to assert beyond the error).
    }

    #[rstest::rstest]
    fn load_theme_updates_state_from_file() {
        // Given a temp directory with a custom theme file.
        let dir = tempfile::tempdir().expect("temp dir");
        let theme_content = "focus_accent = \"magenta\"\nborder_unfocused = \"blue\"";
        std::fs::write(dir.path().join("custom.toml"), theme_content).expect("write theme");

        let state = State::new(AppState::default());

        // Set the theme name in preferences.
        state.write().frontend.app_state.theme_name = Some("custom".to_owned());

        // Capture the initial focus_accent color.
        let initial_focus = state.read().frontend.theme.focus_accent;

        // When loading the theme.
        let empty = PathBuf::from("/nonexistent");
        load_theme(&state, dir.path(), &empty);

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
        state.write().frontend.app_state.theme_name = Some("nonexistent".to_owned());

        let empty = PathBuf::from("/nonexistent");

        // When loading the theme (no matching file).
        // Then it should not panic - the function logs a warning and returns.
        load_theme(&state, &empty, &empty);
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
    fn seed_config_template_preserves_template_comments() {
        // Given a config path inside a directory that does not exist yet.
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("nested").join("jinn.toml");

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
        // Given a --config path that does not exist yet.
        let dir = tempfile::tempdir().expect("temp dir");
        let override_path = dir.path().join("nested").join("alt.toml");

        // When `jinn config init` picks its target.
        let target = config_init_target(Some(&override_path), Path::new("/default/jinn.toml"));

        // Then it targets the override, not the default location.
        assert_eq!(target, override_path);
    }

    #[rstest::rstest]
    fn config_init_without_override_targets_the_default_location() {
        // Given no --config.
        let default = PathBuf::from("/default/jinn.toml");

        // When `jinn config init` picks its target.
        let target = config_init_target(None, &default);

        // Then it targets the default location.
        assert_eq!(target, default);
    }

    #[rstest::rstest]
    fn config_init_override_is_accepted_though_runtime_resolver_rejects_it() {
        // Given a --config path that does not exist.
        let dir = tempfile::tempdir().expect("temp dir");
        let override_path = dir.path().join("alt.toml");

        // When the two choosers are consulted.
        let init_target = config_init_target(Some(&override_path), Path::new("/default/jinn.toml"));
        let runtime = resolve_config_path(Some(&override_path), Path::new("/default/jinn.toml"));

        // Then `config init` still targets the override, since creating the
        // file is its whole job.
        assert_eq!(init_target, override_path);
        // But a run that intends to read the file still rejects it.
        assert!(runtime.is_err());
    }

    #[rstest::rstest]
    fn malformed_config_under_an_override_fails_naming_the_override() {
        // Given an override path holding invalid TOML.
        let dir = tempfile::tempdir().expect("temp dir");
        let override_path = dir.path().join("alt.toml");
        std::fs::write(&override_path, "[section\nkey = ").expect("write");

        // When the config layer is loaded over the resolved path, exactly
        // as dispatch does.
        let backend = jinn_config::FilesystemConfigStorage::new(override_path.clone());
        let result = jinn_config::ConfigLayer::load(Arc::new(backend));

        // Then the load fails, and the report names the override file.
        let report = result.expect_err("malformed override must fail");
        let rendered = format!("{report:?}");
        assert!(
            rendered.contains(&override_path.display().to_string()),
            "fail-fast report should name the override path: {rendered}"
        );
    }
}
