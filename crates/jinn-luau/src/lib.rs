//! Embedded Luau scripting for jinn — the `jinn luau` subcommand family.
//!
//! jinn carries its own Lua dialect so an agent on a lean host (no python,
//! no node, no jq, no shell pipes) can still write and run a script: the
//! interpreter rides inside the jinn binary through mlua's vendored Luau
//! build, and the subcommand runs with no config, no session store, and no
//! running jinn instance.
//!
//! Scripts see one injected table, `jinn`, and the standard library mlua's
//! sandbox leaves in place (`print`, `string`, `table`, `math`, `os.clock`,
//! `os.time`, `coroutine`, plus file-based `require` rooted at the script's
//! own directory). The capability families on the `jinn` table:
//!
//! - `jinn.args` — the argv after `--`
//! - `jinn.fs` — read / write / exists / list_dir / remove
//! - `jinn.env` — get / set
//! - `jinn.http` — request (blocking, on the process's rustls provider)
//! - `jinn.proc` — run (a child process, no shell)
//! - `jinn.stdio` — read (stdin) / eprint (stderr)
//! - `jinn.json` / `jinn.toml` — encode / decode
//! - `jinn.sleep` / `jinn.exit` — wait / terminate with a code
//!
//! The guardrails (a wall-clock timeout and a memory cap) kill agent-written
//! runaway scripts; they are not a security boundary, and the surface is
//! deliberately maximal.
//!
//! Entry points: [`run`] executes a script file or an eval string and maps
//! the outcome to an exit code; [`check`] syntax-checks files through the
//! vendored compiler without executing them; [`version`] reports the jinn
//! and vendored Luau versions.

pub mod caps;
pub mod commands;
pub mod runtime;

use std::path::Path;

use error_stack::Report;
use wherror::Error;

pub use commands::RunMode;
pub use commands::{check, eval, run, version};

/// Installs the process-default rustls crypto provider (ring), no-op when
/// another installer won the race.
///
/// The workspace pins `rustls-no-provider` so every entry point that may
/// build a reqwest client installs the provider first — `main` in the
/// binary, and every test that constructs a [`crate::caps::Caps`] (whose
/// constructor spawns reqwest's blocking event loop, which panics without
/// a provider). Public so library-crate test modules can call one shared
/// installer instead of re-deriving it.
pub fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// How a luau subcommand failed before or during execution.
///
/// Every variant renders a single-sentence user-facing message; the caller
/// prints it to stderr and maps it to an exit code (see [`ExitStatus`]).
#[derive(Debug, Error)]
#[error(debug)]
pub enum LuauError {
    /// The script file could not be read.
    ScriptRead {
        /// The path the read attempted.
        path: String,
    },
    /// The timeout killed the script.
    Timeout {
        /// The budget that elapsed, in seconds.
        budget_secs: u64,
    },
    /// The memory cap killed the script.
    Memory,
    /// The script raised a syntax error.
    Syntax {
        /// The chunk (script path or `=(eval)`) the error belongs to.
        chunk: String,
        /// The compiler's message, already carrying its line position.
        message: String,
    },
    /// The script raised a runtime error.
    ScriptError {
        /// The chunk (script path or `=(eval)`) the error belongs to.
        chunk: String,
        /// The VM's error message.
        message: String,
    },
    /// The subcommand's own setup failed (capability registration, a
    /// malformed request table, an unreadable directory).
    Harness {
        /// What went wrong, as a full sentence.
        sentence: String,
    },
}

/// The exit code a luau subcommand should terminate the process with.
///
/// The mapping is the tool's composable contract: GNU timeout convention
/// for the budget (124), the script's own choice when it exits or returns
/// an integer, 1 for script errors, 2 for harness errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExitStatus(pub i32);

impl LuauError {
    /// The sentence printed to stderr before the non-zero exit.
    #[must_use]
    pub fn sentence(&self) -> String {
        match self {
            Self::ScriptRead { path } => format!("cannot read script `{path}`"),
            Self::Timeout { budget_secs } => {
                format!("script exceeded its {budget_secs}s wall-clock budget (--timeout)")
            }
            Self::Memory => "script exceeded the memory cap".to_owned(),
            // mlua's syntax/runtime messages already carry the `@chunk:line:`
            // position prefix, so the chunk name is not prepended again.
            Self::Syntax { message, .. } | Self::ScriptError { message, .. } => {
                message.trim_start().to_owned()
            }
            Self::Harness { sentence } => sentence.clone(),
        }
    }

    /// The exit code this error maps to.
    ///
    /// Timeout is 124 (the GNU convention, so `jinn luau run` composes with
    /// tooling that already understands it); everything the script did
    /// wrong is 1; everything the harness did wrong is 2 — an unreadable
    /// script is harness trouble (the caller named a wrong path), not a
    /// verdict on script content.
    #[must_use]
    pub fn exit_code(&self) -> ExitStatus {
        match self {
            Self::Timeout { .. } => ExitStatus(124),
            Self::Harness { .. } | Self::ScriptRead { .. } => ExitStatus(2),
            Self::Memory | Self::Syntax { .. } | Self::ScriptError { .. } => ExitStatus(1),
        }
    }
}

/// The wall-clock budget for script execution.
///
/// Absent means the default budget (30s); [`Self::Disabled`] (`--timeout 0`)
/// runs the script unbounded. The three states are distinct because a
/// caller who asks for no timeout must not silently get the default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Timeout {
    /// The default budget: 30 seconds.
    Default,
    /// An explicit budget, in seconds.
    Secs(u64),
    /// No wall-clock budget at all (`--timeout 0`).
    Disabled,
}

impl Timeout {
    /// The budget in seconds, or `None` when disabled.
    ///
    /// The default (and any explicit zero other than the disabled sentinel)
    /// floors at one second so a budget of `--timeout 0` remains the only
    /// unbounded spelling.
    #[must_use]
    pub fn seconds(self) -> Option<u64> {
        match self {
            Self::Default | Self::Secs(0) => Some(30),
            Self::Secs(n) => Some(n.max(1)),
            Self::Disabled => None,
        }
    }
}

/// The script source plus the chunk name the VM should carry it under.
///
/// The chunk name is load-bearing twice: Luau error messages position
/// against it, and mlua's file-based `require` only resolves from chunk
/// names carrying the `@` file-path prefix — so a run's chunk name is the
/// script's absolute path and an eval's is the sentinel `=(eval)`.
#[derive(Debug, Clone)]
pub struct Chunk {
    /// The Luau source text.
    pub source: String,
    /// The chunk name: an `@`-prefixed path for a script file, `=(eval)`
    /// for an eval string.
    pub name: String,
}

/// Reads a script file into a [`Chunk`] named by its absolute path.
///
/// # Errors
///
/// Returns [`LuauError::ScriptRead`] when the file cannot be read.
pub fn chunk_from_file(path: &Path) -> Result<Chunk, Report<LuauError>> {
    use error_stack::ResultExt as _;

    let displayed = path.display().to_string();
    let source = std::fs::read_to_string(path)
        .change_context(LuauError::ScriptRead { path: displayed })
        .attach("failed to read the script file")?;
    Ok(Chunk {
        source,
        name: format!("@{}", path.display()),
    })
}

/// Wraps an eval string into a [`Chunk`] named `=(eval)`.
#[must_use]
pub fn chunk_from_eval(source: String) -> Chunk {
    Chunk {
        source,
        name: "=(eval)".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        reason = "test assertions need expect/panic to state what went wrong"
    )]

    use super::*;
    use marked_path::MarkedPathAccess as _;

    #[rstest::rstest]
    #[test]
    fn error_sentences_name_the_condition() {
        // Given each error variant.
        let errors = [
            LuauError::ScriptRead {
                path: "/tmp/s.luau".to_owned(),
            },
            LuauError::Timeout { budget_secs: 30 },
            LuauError::Memory,
            LuauError::Syntax {
                chunk: "/tmp/s.luau".to_owned(),
                message: ":2: expected identifier".to_owned(),
            },
            LuauError::ScriptError {
                chunk: "=(eval)".to_owned(),
                message: "attempt to call a nil value".to_owned(),
            },
            LuauError::Harness {
                sentence: "boom".to_owned(),
            },
        ];

        // Then every sentence names its condition.
        for error in &errors {
            assert_ne!(error.sentence(), "");
        }
        assert!(errors[0].sentence().contains("/tmp/s.luau"));
        assert!(errors[1].sentence().contains("30s"));
        assert!(errors[3].sentence().contains(":2:"));
    }

    #[rstest::rstest]
    #[test]
    fn timeout_errors_map_to_the_gnu_timeout_code() {
        // Given a timeout error.
        let error = LuauError::Timeout { budget_secs: 5 };

        // Then its exit code is 124.
        assert_eq!(error.exit_code(), ExitStatus(124));
    }

    #[rstest::rstest]
    #[test]
    fn script_errors_map_to_one_and_harness_errors_to_two() {
        // Given one script-side error and one harness-side error.
        let script = LuauError::Memory;
        let harness = LuauError::Harness {
            sentence: "no".to_owned(),
        };

        // Then the script side exits 1 and the harness side exits 2.
        assert_eq!(script.exit_code(), ExitStatus(1));
        assert_eq!(harness.exit_code(), ExitStatus(2));
    }

    #[rstest::rstest]
    #[test]
    fn an_unreadable_script_is_a_harness_error() {
        // Given a script-read failure (the caller named a wrong path).
        let error = LuauError::ScriptRead {
            path: "/tmp/nope.luau".to_owned(),
        };

        // Then it exits 2, not the script's 1.
        assert_eq!(error.exit_code(), ExitStatus(2));
    }

    #[rstest::rstest]
    #[test]
    fn script_error_sentences_do_not_repeat_the_chunk_name() {
        // Given a runtime error whose message mlua already positioned
        // (`@/path/x.luau:1: boom` minus the position it strips).
        let error = LuauError::ScriptError {
            chunk: "@/tmp/x.luau".to_owned(),
            message: "@/tmp/x.luau:1: boom".to_owned(),
        };

        // Then the sentence is the message alone — the chunk is not
        // prepended a second time.
        assert_eq!(error.sentence(), "@/tmp/x.luau:1: boom");
    }

    #[rstest::rstest]
    #[test]
    fn timeout_default_yields_the_thirty_second_budget() {
        // Given the default timeout.
        // When reading its budget.
        // Then it is thirty seconds.
        assert_eq!(Timeout::Default.seconds(), Some(30));
    }

    #[rstest::rstest]
    #[test]
    fn timeout_disabled_yields_no_budget() {
        // Given the disabled timeout (`--timeout 0`).
        // When reading its budget.
        // Then there is none.
        assert_eq!(Timeout::Disabled.seconds(), None);
    }

    #[rstest::rstest]
    #[test]
    fn timeout_secs_pass_through_and_floor_at_one_second() {
        // Given explicit budgets, one normal and one degenerate.
        // When reading each budget.
        // Then the normal one passes through and the degenerate floors at 1.
        assert_eq!(Timeout::Secs(5).seconds(), Some(5));
        assert_eq!(Timeout::Secs(0).seconds(), Some(30));
    }

    #[rstest::rstest]
    #[test]
    fn chunk_from_file_names_the_chunk_by_absolute_path() {
        // Given a script file on disk.
        let root = jinn_testutil::TestRoot::new();
        let path = root.join_str("s.luau").expect("joins");
        std::fs::write(path.as_path(), "return 1").expect("write");

        // When reading it into a chunk.
        let chunk = chunk_from_file(path.as_path()).expect("reads");

        // Then the chunk name carries the @ prefix and the path.
        assert!(chunk.name.starts_with('@'));
        assert!(chunk.name.contains("s.luau"));
        assert_eq!(chunk.source, "return 1");
    }

    #[rstest::rstest]
    #[test]
    fn chunk_from_file_names_the_missing_file_error() {
        // Given a path that names no file.
        let root = jinn_testutil::TestRoot::new();
        let path = root.join_str("absent.luau").expect("joins");

        // When reading it into a chunk.
        let result = chunk_from_file(path.as_path());

        // Then the read error names the path.
        let report = result.expect_err("absent file");
        assert!(report.current_context().sentence().contains("absent.luau"));
    }

    #[rstest::rstest]
    #[test]
    fn chunk_from_eval_uses_the_eval_sentinel_name() {
        // Given an eval string.
        // When wrapping it.
        let chunk = chunk_from_eval("return 1".to_owned());

        // Then the chunk name is the eval sentinel.
        assert_eq!(chunk.name, "=(eval)");
    }
}
