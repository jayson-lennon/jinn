//! The `jinn luau` intercept entry — where the subcommand family runs and
//! the process terminates.
//!
//! This module owns the one thing no other command path may do: exit with
//! an arbitrary script-chosen code. A script runner's contract is that the
//! script's exit code, the timeout's 124, and the harness's 2 all reach
//! the shell, so the runner prints its outcome, flushes both streams, and
//! calls `std::process::exit` itself instead of returning through
//! `App::dispatch`'s single error path (the `run_dictation_subcommand`
//! precedent, generalized).
//!
//! Like `jinn remote`, the intercept happens in `prepare` before any
//! config read or session-store open: a lean host with no `jinn.toml`
//! must be able to run a script.

use jinn_cli::cli::Commands;
use jinn_cli::cli::LuauCommands;
use jinn_luau::Timeout;

/// Runs the `jinn luau` subcommand and terminates the process with the
/// mapped exit code. Never returns.
///
/// Called from `prepare` before any config read; every failure prints one
/// sentence to stderr (`LuauError::sentence`) and exits with
/// [`jinn_luau::LuauError::exit_code`].
#[expect(
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::exit,
    reason = "the `jinn luau` subcommand IS the process: it prints its outcome and exits with the script's own code; no TUI ever starts"
)]
pub fn run(command: &Commands) -> ! {
    let Commands::Luau {
        eval,
        timeout,
        subcommand,
    } = command
    else {
        // The prepare() intercept pattern-matches Commands::Luau before
        // calling here, so any other variant is a wiring bug; exit cleanly
        // as harness error 2 rather than panicking.
        eprintln!("jinn luau called with a non-luau command");
        let _ = std::io::Write::flush(&mut std::io::stderr());
        std::process::exit(2);
    };

    let spec = timeout.map_or(Timeout::Default, |secs| {
        if secs == 0 {
            Timeout::Disabled
        } else {
            Timeout::Secs(secs)
        }
    });

    let outcome = match subcommand {
        Some(LuauCommands::Run { script, args }) => {
            // Resolve to the typed owning form here (the CLI boundary) with
            // the shared `--log-file` rules: relative against the process
            // cwd, tilde against home; a resolution failure is the harness
            // error path below.
            let home = jinn_kernel::AppPaths::default();
            let resolved = crate::cli_paths::resolve_cli_path("--script", script, home.home_dir())
                .map_err(|error| {
                    error_stack::Report::new(jinn_luau::LuauError::Harness {
                        sentence: format!(
                            "cannot resolve the script path `{}`: {error}",
                            script.display()
                        ),
                    })
                });
            match resolved {
                Ok(path) => jinn_luau::run(&jinn_luau::RunMode::Script(path), args.clone(), spec),
                Err(report) => Err(report),
            }
        }
        Some(LuauCommands::Check { scripts }) => {
            let home = jinn_kernel::AppPaths::default();
            let resolved: Result<Vec<_>, _> = scripts
                .iter()
                .map(|script| {
                    crate::cli_paths::resolve_cli_path("--script", script, home.home_dir())
                })
                .collect();
            match resolved {
                Ok(paths) => jinn_luau::check(&paths),
                Err(error) => Err(error_stack::Report::new(jinn_luau::LuauError::Harness {
                    sentence: format!("cannot resolve a script path: {error}"),
                })),
            }
        }
        Some(LuauCommands::Version) => {
            let outcome = jinn_luau::version(env!("CARGO_PKG_VERSION"));
            for line in &outcome.lines {
                println!("{line}");
            }
            let _ = std::io::Write::flush(&mut std::io::stdout());
            std::process::exit(outcome.status.0);
        }
        // The clap conflict group makes --eval + subcommand unparseable;
        // reaching this arm with both set is impossible, so eval only
        // runs when no subcommand exists at all.
        None => match eval {
            Some(source) => jinn_luau::eval(source.clone(), spec),
            None => {
                eprintln!(
                    "nothing to run: pass a script (`jinn luau run <script>`), --eval '<code>', or a subcommand"
                );
                std::process::exit(2);
            }
        },
    };

    match outcome {
        Ok(outcome) => {
            for line in &outcome.lines {
                println!("{line}");
            }
            let _ = std::io::Write::flush(&mut std::io::stdout());
            std::process::exit(outcome.status.0);
        }
        Err(report) => {
            let error = report.current_context();
            eprintln!("{}", error.sentence());
            let _ = std::io::Write::flush(&mut std::io::stderr());
            std::process::exit(error.exit_code().0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[rstest::rstest]
    #[test]
    fn timeout_zero_maps_to_the_disabled_spec() {
        // Given a `--timeout 0` value.
        // When converting to the spec.
        let spec = Some(0u64).map_or(Timeout::Default, |secs| {
            if secs == 0 {
                Timeout::Disabled
            } else {
                Timeout::Secs(secs)
            }
        });

        // Then no budget applies.
        assert_eq!(spec, Timeout::Disabled);
        assert_eq!(spec.seconds(), None);
    }

    #[rstest::rstest]
    #[test]
    fn an_absent_timeout_maps_to_the_default_spec() {
        // Given no --timeout flag.
        // When converting.
        let spec = None::<u64>.map_or(Timeout::Default, |secs| {
            if secs == 0 {
                Timeout::Disabled
            } else {
                Timeout::Secs(secs)
            }
        });

        // Then the default budget applies.
        assert_eq!(spec, Timeout::Default);
        assert_eq!(spec.seconds(), Some(30));
    }

    #[rstest::rstest]
    #[test]
    fn an_explicit_timeout_maps_to_the_secs_spec() {
        // Given a `--timeout 5` value.
        // When converting.
        let spec = Some(5u64).map_or(Timeout::Default, |secs| {
            if secs == 0 {
                Timeout::Disabled
            } else {
                Timeout::Secs(secs)
            }
        });

        // Then the five-second budget applies.
        assert_eq!(spec, Timeout::Secs(5));
        assert_eq!(spec.seconds(), Some(5));
    }
}
