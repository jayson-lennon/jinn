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
use jinn_luau::commands::CommandOutcome;
use jinn_luau::Timeout;

/// Resolves a script path argument the way every luau subcommand does:
/// relative against the process cwd, tilde against home, with a failure
/// mapped to the harness error naming the flag and the value.
fn resolve_script_arg(
    script: &std::path::Path,
) -> Result<
    marked_path::MarkedPathBuf<marked_path::Absolute>,
    error_stack::Report<jinn_luau::LuauError>,
> {
    let home = jinn_kernel::AppPaths::default();
    crate::cli_paths::resolve_cli_path("--script", script, home.home_dir()).map_err(|error| {
        error_stack::Report::new(jinn_luau::LuauError::Harness {
            sentence: format!("cannot resolve the script path `{}`: {error}", script.display()),
        })
    })
}

/// Flushes both streams and terminates the process with `code`.
///
/// The flushes are best-effort: the exit code, not the flush result, is
/// the function's contract.
#[expect(
    clippy::exit,
    reason = "the `jinn luau` subcommand IS the process: it terminates with the script's own code; no TUI ever starts"
)]
fn exit_flushing(code: i32) -> ! {
    let _ = std::io::Write::flush(&mut std::io::stdout());
    let _ = std::io::Write::flush(&mut std::io::stderr());
    std::process::exit(code);
}

/// Prints a successful [`CommandOutcome`] and terminates with its status.
#[expect(
    clippy::print_stdout,
    reason = "outcome lines are the subcommand's stdout contract"
)]
fn exit_printing(outcome: &CommandOutcome) -> ! {
    for line in &outcome.lines {
        println!("{line}");
    }
    exit_flushing(outcome.status.0)
}

/// Prints one error sentence and terminates with the error's own code.
#[expect(
    clippy::print_stderr,
    reason = "the one-sentence stderr render is the luau error path's contract"
)]
fn exit_with_error(report: &error_stack::Report<jinn_luau::LuauError>) -> ! {
    let error = report.current_context();
    eprintln!("{}", error.sentence());
    exit_flushing(error.exit_code().0)
}

/// Prints one harness sentence and terminates with the harness code 2.
#[expect(
    clippy::print_stderr,
    reason = "the one-sentence stderr render is the luau error path's contract"
)]
fn exit_harness(sentence: &str) -> ! {
    eprintln!("{sentence}");
    exit_flushing(2)
}

/// Runs the `jinn luau` subcommand and terminates the process with the
/// mapped exit code. Never returns.
///
/// Called from `prepare` before any config read; every failure prints one
/// sentence to stderr (`LuauError::sentence`) and exits with
/// [`jinn_luau::LuauError::exit_code`].
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
        exit_harness("jinn luau called with a non-luau command");
    };

    let spec = timeout.map_or(Timeout::Default, |secs| {
        if secs == 0 {
            Timeout::Disabled
        } else {
            Timeout::Secs(secs)
        }
    });

    // Serve exits on its own path: it blocks until the client disconnects
    // (success = exit 0) or fails setup (error path below). It never
    // produces a CommandOutcome.
    if let Some(LuauCommands::Serve { script, args }) = subcommand {
        match resolve_script_arg(script)
            .and_then(|path| jinn_luau::serve(&path.into_inner(), args.clone(), spec))
        {
            Ok(()) => exit_flushing(0),
            Err(report) => exit_with_error(&report),
        }
    }

    let outcome = match subcommand {
        Some(LuauCommands::Run { script, args }) => resolve_script_arg(script).and_then(|path| {
            jinn_luau::run(&jinn_luau::RunMode::Script(path), args.clone(), spec)
        }),
        Some(LuauCommands::Check { scripts }) => scripts
            .iter()
            .map(|script| resolve_script_arg(script))
            .collect::<Result<Vec<_>, _>>()
            .and_then(|paths| jinn_luau::check(&paths)),
        Some(LuauCommands::Version) => {
            exit_printing(&jinn_luau::version(env!("CARGO_PKG_VERSION")));
        }
        // The clap conflict group makes --eval + subcommand unparseable;
        // reaching this arm with both set is impossible, so eval only
        // runs when no subcommand exists at all.
        None => match eval {
            Some(source) => jinn_luau::eval(source.clone(), spec),
            None => exit_harness(
                "nothing to run: pass a script (`jinn luau run <script>`), --eval '<code>', or a subcommand",
            ),
        },
        // Serve exited on its own path above; this arm exists only to
        // keep the match exhaustive.
        Some(LuauCommands::Serve { .. }) => {
            exit_harness("jinn luau serve handled on its own exit path (a wiring bug)");
        }
    };

    match outcome {
        Ok(outcome) => exit_printing(&outcome),
        Err(report) => exit_with_error(&report),
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

    #[rstest::rstest]
    #[test]
    #[expect(
        clippy::disallowed_types,
        reason = "argv-shaped fixture strings at the resolve seam; the owned PathBuf never leaves the test"
    )]
    fn serve_of_an_unresolvable_script_maps_to_the_harness_error() {
        // Given a serve argv whose script path cannot resolve — a
        // relative path, and the cwd read fails (the injected-supplier
        // shape of `resolve_cli_path_in`; the arm applies the identical
        // map_err around `resolve_cli_path`).
        let dir = jinn_testutil::tempdir();
        let script = std::path::PathBuf::from("s.luau");
        let cwd_read_fails = || -> std::io::Result<std::path::PathBuf> {
            Err(std::io::Error::other("cwd unavailable"))
        };

        // When resolving the way the serve arm does.
        let home = jinn_kernel::AppPaths::default();
        let resolved = crate::cli_paths::resolve_cli_path_in(
            "--script",
            script.as_path(),
            home.home_dir(),
            cwd_read_fails,
        );
        let report = resolved.map_err(|error| {
            error_stack::Report::new(jinn_luau::LuauError::Harness {
                sentence: format!(
                    "cannot resolve the script path `{}`: {error}",
                    script.display()
                ),
            })
        });

        // Then the failure is the harness error naming the path, which
        // the intercept's error path prints and exits 2 on.
        let report = report.unwrap_err();
        let error = report.current_context();
        assert!(matches!(error, jinn_luau::LuauError::Harness { .. }));
        assert!(
            error.sentence().contains("cannot resolve the script path"),
            "got {}",
            error.sentence()
        );
        let _ = dir;
    }
}
