//! Binary entry point for jinn.

use clap::Parser;
use jinn::tracing::{TracingMode, init as init_tracing};
use jinn_cli::Cli;

fn main() {
    // TLS clients (reqwest) need a rustls provider; `rustls-no-provider` is in
    // effect, so install ring before anything builds a client.
    jinn::install_crypto_provider();

    // Load .env if present. Not fatal if missing.
    let _ = dotenvy::dotenv();

    let cli = Cli::parse();

    // Debug-build guard: `--db-path` is required for session-store commands.
    // Enforced post-parse (not by clap) so bundled MCP servers stay spawnable
    // without the flag — they never open the session store.
    if let Err(e) = cli.debug_db_path_required() {
        eprintln!("error: {e}");
        std::process::exit(2);
    }

    let paths = jinn_kernel::AppPaths::default();
    let log_path = match cli.log_file.as_deref() {
        Some(file) => match jinn::cli_paths::resolve_cli_path("--log-file", file, paths.home_dir())
        {
            Ok(typed) => typed,
            Err(report) => {
                eprintln!("error: {report:?}");
                std::process::exit(2);
            }
        },
        None => paths.log_path(),
    };

    let mode = match &cli.command {
        None
        | Some(jinn_cli::cli::Commands::Tui)
        | Some(jinn_cli::cli::Commands::Completions { .. }) => TracingMode::Tui { log_path },
        Some(jinn_cli::cli::Commands::Fetch { .. })
        | Some(jinn_cli::cli::Commands::Dictation { .. })
        | Some(jinn_cli::cli::Commands::Config { .. })
        | Some(jinn_cli::cli::Commands::Install { .. })
        | Some(jinn_cli::cli::Commands::Mcp { .. })
        | Some(jinn_cli::cli::Commands::Remote { .. }) => TracingMode::Headless { log_path },
        #[cfg(debug_assertions)]
        Some(jinn_cli::cli::Commands::Headless { .. }) => TracingMode::Headless { log_path },
    };

    if let Err(e) = init_tracing(cli.verbosity, mode, cli.trace_color, cli.log_tail_lines) {
        eprintln!("error: {e:?}");
        std::process::exit(1);
    }

    let mut app = match jinn::App::new() {
        Ok(app) => app,
        Err(e) => {
            eprintln!("error: {e:?}");
            std::process::exit(1);
        }
    };

    if let Err(e) = app.dispatch(cli) {
        eprintln!("error: {e:?}");
        std::process::exit(1);
    }
}
