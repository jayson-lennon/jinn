//! The `jinn luau` command implementations: run, eval, check, version.
//!
//! Each entry returns a [`CommandOutcome`]: the lines to print plus the
//! [`ExitStatus`] the caller terminates with. Nothing here exits the
//! process — the root crate's `luau_cli` module owns flushing and
//! `std::process::exit`, because a script runner must land its own
//! non-zero codes (the script's, 124 on timeout, 2 on harness trouble)
//! rather than dispatch's single error path.
//!
//! The outcome type is the test seam: command-level tests run scripts
//! end to end through [`run`]/[`eval`]/[`check`] and assert on the
//! returned status and lines — the same observable surface the process
//! exposes.

use marked_path::MarkedPathAccess as _;
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;
use std::time::Instant;

use error_stack::Report;
use mlua::Error as MluaError;
use mlua::Value;
use mlua::chunk::Compiler;

use crate::ExitStatus;
use crate::LuauError;
use crate::Timeout;
use crate::caps::Caps;
use crate::caps::time::exit_code_from_result;
use crate::chunk_from_eval;
use crate::chunk_from_file;
use crate::runtime::TIMEOUT_SENTINEL;
use crate::runtime::execute;

/// What a luau subcommand produced: output lines and an exit code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutcome {
    /// The lines to print to stdout, in order.
    pub lines: Vec<String>,
    /// The exit code the process should terminate with.
    pub status: ExitStatus,
}

impl CommandOutcome {
    /// A successful outcome with no output.
    #[must_use]
    pub fn silent(status: ExitStatus) -> Self {
        Self {
            lines: Vec::new(),
            status,
        }
    }

    /// A successful outcome printing one line.
    #[must_use]
    pub fn one_line<L: Into<String>>(line: L) -> Self {
        Self {
            lines: vec![line.into()],
            status: ExitStatus(0),
        }
    }
}

/// The mode a run executes in: a script file or an eval string.
#[derive(Debug, Clone)]
pub enum RunMode {
    /// Execute the file at this owning, absolute path.
    Script(marked_path::MarkedPathBuf<marked_path::Absolute>),
    /// Evaluate this source string.
    Eval(String),
}

/// `jinn luau run` / `jinn luau --eval`: execute and map to an exit code.
///
/// The caller captures argv after `--` into `args` and pre-computed the
/// deadline from the `Timeout` spec; this function resolves the script
/// (or eval) chunk, builds the VM with every capability, executes, and
/// maps the outcome.
///
/// # Errors
///
/// Returns the error vocabulary ([`LuauError`]); the caller prints the
/// sentence and exits with [`LuauError::exit_code`].
pub fn run(
    mode: &RunMode,
    args: Vec<String>,
    timeout: Timeout,
) -> Result<CommandOutcome, Report<LuauError>> {
    run_with_budget(mode, args, timeout.seconds().unwrap_or_default())
}

/// The [`run`] body, carrying the resolved budget in seconds so the timeout
/// error sentence can name the number the script was actually given.
fn run_with_budget(
    mode: &RunMode,
    args: Vec<String>,
    budget_secs: u64,
) -> Result<CommandOutcome, Report<LuauError>> {
    // The chunk name carries the require root, so the script's directory
    // is captured only for the error-context wording; eval resolves
    // against the platform temp dir.
    let (chunk, script_dir) = match mode {
        RunMode::Script(path) => {
            let chunk = chunk_from_file(path.as_path())?;
            let dir = path
                .parent()
                .map_or_else(std::env::temp_dir, |parent| parent.to_path_buf());
            (chunk, absolute_dir(&dir))
        }
        RunMode::Eval(source) => (
            chunk_from_eval(source.clone()),
            absolute_dir(&std::env::temp_dir()),
        ),
    };

    let cwd = absolute_dir(&std::env::current_dir().map_err(|error| {
        Report::new(LuauError::Harness {
            sentence: format!("cannot read the process cwd: {error}"),
        })
    })?);
    let deadline = (budget_secs != 0).then(|| Instant::now() + Duration::from_secs(budget_secs));
    let caps = Rc::new(Caps::new(args, cwd, deadline));
    let _ = script_dir;

    let lua = crate::runtime::build(&caps).map_err(Report::new)?;
    let value =
        execute(&lua, &chunk).map_err(|error| script_error(&chunk.name, budget_secs, error))?;

    Ok(CommandOutcome::silent(ExitStatus(exit_code_from_result(
        &value,
    ))))
}

/// `jinn luau --eval '<code>'`: evaluate and print the result.
///
/// The chunk executes under the `=(eval)` name; a non-nil return value is
/// printed (integers bare, strings bare, tables via Luau's tostring), and
/// an integer return still maps to the process exit code.
///
/// # Errors
///
/// Same vocabulary as [`run`].
pub fn eval(source: String, timeout: Timeout) -> Result<CommandOutcome, Report<LuauError>> {
    eval_with_budget(source, timeout.seconds().unwrap_or_default())
}

/// The [`eval`] body, carrying the resolved budget in seconds for the same
/// reason as [`run_with_budget`].
fn eval_with_budget(source: String, budget_secs: u64) -> Result<CommandOutcome, Report<LuauError>> {
    let chunk = chunk_from_eval(source);
    let cwd = absolute_dir(&std::env::current_dir().map_err(|error| {
        Report::new(LuauError::Harness {
            sentence: format!("cannot read the process cwd: {error}"),
        })
    })?);
    let deadline = (budget_secs != 0).then(|| Instant::now() + Duration::from_secs(budget_secs));
    let caps = Rc::new(Caps::new(Vec::new(), cwd, deadline));

    let lua = crate::runtime::build(&caps).map_err(Report::new)?;
    let value =
        execute(&lua, &chunk).map_err(|error| script_error(&chunk.name, budget_secs, error))?;
    let status = ExitStatus(exit_code_from_result(&value));

    if value == Value::Nil {
        return Ok(CommandOutcome {
            lines: Vec::new(),
            status,
        });
    }
    let printed = match &value {
        Value::Integer(code) => code.to_string(),
        Value::Number(f) => f.to_string(),
        Value::Boolean(b) => b.to_string(),
        Value::String(s) => s.to_string_lossy(),
        _ => {
            // Tables etc.: render through Lua's tostring so metatables apply.
            let lua = &lua;
            let function: mlua::Function = lua
                .globals()
                .get("tostring")
                .map_err(|error| script_error(&chunk.name, budget_secs, error))?;
            function
                .call::<String>(value.clone())
                .map_err(|error| script_error(&chunk.name, budget_secs, error))?
        }
    };
    Ok(CommandOutcome {
        lines: vec![printed],
        status,
    })
}

/// `jinn luau check <script>...`: syntax-gate each file, executing nothing.
///
/// Compilation goes through the vendored Luau compiler directly — no VM
/// is built. Every failure reports `path:line: message`; the outcome is
/// success only when every file compiles.
///
/// # Errors
///
/// Returns [`LuauError::ScriptRead`] when a file cannot be read.
pub fn check(
    scripts: &[marked_path::MarkedPathBuf<marked_path::Absolute>],
) -> Result<CommandOutcome, Report<LuauError>> {
    let mut lines = Vec::new();
    let mut failed = false;
    for path in scripts {
        let chunk = chunk_from_file(path.as_path())?;
        if let Err(error) = Compiler::new().compile(chunk.source.as_bytes()) {
            failed = true;
            lines.push(format!(
                "{}:{}",
                path.as_path().display(),
                syntax_message(&error)
            ));
        }
    }
    if !failed {
        lines.push("ok".to_owned());
    }
    let status = i32::from(failed);
    Ok(CommandOutcome {
        lines,
        status: ExitStatus(status),
    })
}

/// `jinn luau version`: print the jinn and vendored Luau versions.
#[must_use]
pub fn version(jinn_version: &str) -> CommandOutcome {
    let luau_version = {
        let lua = mlua::Lua::new();
        let _ = lua.sandbox(true);
        lua.globals()
            .get::<String>("_VERSION")
            .unwrap_or_else(|_| "Luau (unknown)".to_owned())
    };
    CommandOutcome::one_line(format!("jinn {jinn_version} ({luau_version})"))
}

/// Maps a script execution error into the error vocabulary.
///
/// The timeout sentinel (raised by the VM interrupt) maps to the timeout
/// variant; syntax, memory, and runtime errors map to their own variants;
/// anything else is harness-side.
fn script_error(chunk: &str, budget_secs: u64, error: MluaError) -> Report<LuauError> {
    match error {
        MluaError::RuntimeError(message) if message.contains(TIMEOUT_SENTINEL) => {
            Report::new(LuauError::Timeout { budget_secs })
        }
        MluaError::SyntaxError { message, .. } => Report::new(LuauError::Syntax {
            chunk: chunk.to_owned(),
            message,
        }),
        MluaError::MemoryError(_) => Report::new(LuauError::Memory),
        MluaError::RuntimeError(message) => Report::new(LuauError::ScriptError {
            chunk: chunk.to_owned(),
            message,
        }),
        other => Report::new(LuauError::Harness {
            sentence: format!("{other}"),
        }),
    }
}

/// The compiler's message, already carrying its `:line:` position.
fn syntax_message(error: &MluaError) -> String {
    match error {
        MluaError::SyntaxError { message, .. } => message.clone(),
        other => other.to_string(),
    }
}

/// Absolutizes a directory path into the typed owning form `Caps` carries;
/// a failure falls back to the platform temp dir so the run still proceeds.
#[expect(
    clippy::expect_used,
    reason = "std::env::temp_dir is absolute by contract; the expects guard typed construction, not real failure modes"
)]
fn absolute_dir(dir: &Path) -> marked_path::MarkedPathBuf<marked_path::Absolute> {
    if let Ok(typed) = marked_path::MarkedPathBuf::<marked_path::Absolute>::new(dir) {
        return typed;
    }
    marked_path::MarkedPathBuf::<marked_path::Absolute>::new(std::env::temp_dir())
        .expect("temp_dir is absolute")
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unwrap_used,
        reason = "test assertions need expect/panic to state what went wrong"
    )]

    use super::*;

    /// Installs the ring TLS provider once per process, the same one-line
    /// install `main` runs — reqwest's blocking client panics at
    /// construction without it.
    fn install_crypto_provider() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }

    /// Writes one script file into a fresh temp dir, returning the dir.
    fn script_file(
        contents: &str,
    ) -> (
        jinn_testutil::temp::JinnTempDir,
        marked_path::MarkedPathBuf<marked_path::Absolute>,
    ) {
        install_crypto_provider();
        let dir = jinn_testutil::tempdir();
        let path = {
            dir.path.join_relative(
                &marked_path::MarkedPathBuf::<marked_path::Relative>::new("s.luau")
                    .expect("relative name")
                    .as_marked_path(),
            )
        };
        std::fs::write(&path, contents).expect("write script");
        (dir, path)
    }

    #[rstest::rstest]
    #[test]
    fn run_maps_an_integer_chunk_return_to_the_exit_code() {
        install_crypto_provider();
        // Given a script returning 3.
        let (_dir, path) = script_file("return 3");

        // When running it.
        let outcome =
            run(&RunMode::Script(path.clone()), Vec::new(), Timeout::Default).expect("runs");

        // Then the exit code is the script's.
        assert_eq!(outcome.status, ExitStatus(3));
    }

    #[rstest::rstest]
    #[test]
    fn run_maps_a_nil_return_to_success() {
        install_crypto_provider();
        // Given a script returning nothing.
        let (_dir, path) = script_file("print('side effect')");

        // When running it.
        let outcome =
            run(&RunMode::Script(path.clone()), Vec::new(), Timeout::Default).expect("runs");

        // Then the exit code is zero.
        assert_eq!(outcome.status, ExitStatus(0));
    }

    #[rstest::rstest]
    #[test]
    fn run_sees_the_caller_supplied_args() {
        install_crypto_provider();
        // Given a script that fails unless jinn.args carries the probe word.
        let (_dir, path) =
            script_file("assert(jinn.args[1] == 'probe-arg', 'missing arg') return 0");

        // When running it with the argv entry.
        let outcome = run(
            &RunMode::Script(path.clone()),
            vec!["probe-arg".to_owned()],
            Timeout::Default,
        )
        .expect("runs");

        // Then it exits zero, proving the argv landed.
        assert_eq!(outcome.status, ExitStatus(0));
    }

    #[rstest::rstest]
    #[test]
    fn run_of_a_syntax_error_names_the_position() {
        install_crypto_provider();
        // Given a script with a syntax error on line 2.
        let (_dir, path) = script_file("local ok = 1\nlocal x = = 2");

        // When running it.
        let report = run(&RunMode::Script(path.clone()), Vec::new(), Timeout::Default)
            .expect_err("syntax error");

        // Then the error is the syntax variant carrying the line.
        let error = report.current_context();
        assert!(matches!(error, LuauError::Syntax { .. }));
        assert!(error.sentence().contains(":2:"), "got {}", error.sentence());
    }

    #[rstest::rstest]
    #[test]
    fn run_maps_a_runtime_error_to_one() {
        install_crypto_provider();
        // Given a script that raises at runtime.
        let (_dir, path) = script_file("error('script-side failure')");

        // When running it.
        let report = run(&RunMode::Script(path.clone()), Vec::new(), Timeout::Default)
            .expect_err("runtime error");

        // Then the error is the script variant naming the message.
        let error = report.current_context();
        assert!(matches!(error, LuauError::ScriptError { .. }));
        assert!(error.sentence().contains("script-side failure"));
    }

    #[rstest::rstest]
    #[test]
    fn run_kills_a_runaway_loop_at_the_timeout() {
        install_crypto_provider();
        // Given an infinite-loop script and a one-second budget.
        let (_dir, path) = script_file("while true do end");

        // When running it.
        let report =
            run(&RunMode::Script(path.clone()), Vec::new(), Timeout::Secs(1)).expect_err("timeout");

        // Then the timeout variant comes back.
        assert!(matches!(
            report.current_context(),
            LuauError::Timeout { .. }
        ));
    }

    #[rstest::rstest]
    #[test]
    fn run_kills_an_allocation_hog_at_the_cap() {
        install_crypto_provider();
        // Given an unbounded allocation script (integer pushes grow the
        // table fast without multi-megabyte memcpy churn, so the cap fires
        // in about a second — string-rep variants take minutes to wedge
        // through realloc cycles before the same cap trips).
        let (_dir, path) = script_file("local t = {} while true do t[#t+1] = 1 end");

        // When running it.
        let report = run(
            &RunMode::Script(path.clone()),
            Vec::new(),
            Timeout::Disabled,
        )
        .expect_err("memory");

        // Then the memory variant comes back.
        assert!(matches!(report.current_context(), LuauError::Memory));
    }

    #[rstest::rstest]
    #[test]
    fn run_with_a_timeout_zero_budget_runs_bounded_work() {
        install_crypto_provider();
        // Given a bounded script under `--timeout 0` semantics.
        // (`Timeout::Secs(0)` floors to the default 30s; `Disabled` is the
        // unbounded spelling — this test pins that bounded work completes
        // under both readings.)
        let (_dir, path) = script_file("local s = 0 for i = 1, 1000 do s += i end return 0");

        // When running with the disabled timeout.
        let outcome = run(
            &RunMode::Script(path.clone()),
            Vec::new(),
            Timeout::Disabled,
        )
        .expect("runs");

        // Then it exits zero.
        assert_eq!(outcome.status, ExitStatus(0));
    }

    #[rstest::rstest]
    #[test]
    fn eval_prints_the_integer_result_and_maps_the_code() {
        install_crypto_provider();
        // Given an eval string returning 5.
        // When evaluating.
        let outcome = eval("return 5".to_owned(), Timeout::Default).expect("evaluates");

        // Then the line is the bare integer and the status carries it.
        assert_eq!(outcome.lines, vec!["5"]);
        assert_eq!(outcome.status, ExitStatus(5));
    }

    #[rstest::rstest]
    #[test]
    fn eval_prints_a_string_result_with_success() {
        install_crypto_provider();
        // Given an eval string returning a string.
        // When evaluating.
        let outcome = eval("return 'hello'".to_owned(), Timeout::Default).expect("evaluates");

        // Then the line is the string and the status is success.
        assert_eq!(outcome.lines, vec!["hello"]);
        assert_eq!(outcome.status, ExitStatus(0));
    }

    #[rstest::rstest]
    #[test]
    fn eval_with_no_return_is_silent_success() {
        install_crypto_provider();
        // Given an eval string that assigns without returning.
        // When evaluating.
        let outcome = eval("local x = 1".to_owned(), Timeout::Default).expect("evaluates");

        // Then nothing prints and the status is success.
        assert_eq!(outcome, CommandOutcome::silent(ExitStatus(0)));
    }

    #[rstest::rstest]
    #[test]
    fn eval_reports_a_syntax_error_with_the_eval_chunk_name() {
        install_crypto_provider();
        // Given a broken eval string.
        // When evaluating.
        let report = eval("return =".to_owned(), Timeout::Default).expect_err("syntax");

        // Then the error names the eval chunk (mlua positions it as
        // `(eval):line:` in the message itself).
        assert!(
            report.current_context().sentence().starts_with("(eval):"),
            "got {}",
            report.current_context().sentence()
        );
    }

    #[rstest::rstest]
    #[test]
    fn eval_sees_the_jinn_table() {
        install_crypto_provider();
        // Given an eval string that reads the capability table.
        // When evaluating a json roundtrip.
        let outcome = eval(
            "return jinn.json.decode('[1,2]')[2]".to_owned(),
            Timeout::Default,
        )
        .expect("evaluates");

        // Then the decoded value comes back.
        assert_eq!(outcome.lines, vec!["2"]);
    }

    #[rstest::rstest]
    #[test]
    fn check_passes_a_valid_script_and_fails_a_broken_one_with_position() {
        // Given one valid script and one with a syntax error on line 3.
        install_crypto_provider();
        let dir = jinn_testutil::tempdir();
        let good: marked_path::MarkedPathBuf<marked_path::Absolute> = {
            dir.path.join_relative(
                &marked_path::MarkedPathBuf::<marked_path::Relative>::new("good.luau")
                    .expect("relative name")
                    .as_marked_path(),
            )
        };
        let bad: marked_path::MarkedPathBuf<marked_path::Absolute> = {
            dir.path.join_relative(
                &marked_path::MarkedPathBuf::<marked_path::Relative>::new("bad.luau")
                    .expect("relative name")
                    .as_marked_path(),
            )
        };
        std::fs::write(&good, "return 1").expect("write");
        std::fs::write(&bad, "local a = 1\nlocal b = 2\nlocal c = = 3").expect("write");

        // When checking both.
        let outcome = check(&[good, bad]).expect("checks");

        // Then the outcome is failure with a line-positioned diagnostic.
        assert_eq!(outcome.status, ExitStatus(1));
        let diagnostic = outcome
            .lines
            .first()
            .expect("the failure pushed one diagnostic");
        assert!(diagnostic.contains("bad.luau"), "got {diagnostic}");
        assert!(diagnostic.contains(":3:"), "got {diagnostic}");
    }

    #[rstest::rstest]
    #[test]
    fn check_all_valid_reports_ok_and_success() {
        // Given two valid scripts.
        install_crypto_provider();
        let dir = jinn_testutil::tempdir();
        let one: marked_path::MarkedPathBuf<marked_path::Absolute> = {
            dir.path.join_relative(
                &marked_path::MarkedPathBuf::<marked_path::Relative>::new("one.luau")
                    .expect("relative name")
                    .as_marked_path(),
            )
        };
        let two: marked_path::MarkedPathBuf<marked_path::Absolute> = {
            dir.path.join_relative(
                &marked_path::MarkedPathBuf::<marked_path::Relative>::new("two.luau")
                    .expect("relative name")
                    .as_marked_path(),
            )
        };
        std::fs::write(&one, "return 1").expect("write");
        std::fs::write(&two, "print('x')").expect("write");

        // When checking them.
        let outcome = check(&[one, two]).expect("checks");

        // Then the outcome is success.
        assert_eq!(outcome.status, ExitStatus(0));
    }

    #[rstest::rstest]
    #[test]
    fn check_of_a_missing_file_is_a_read_error() {
        // Given a path naming no file.
        install_crypto_provider();
        let dir = jinn_testutil::tempdir();
        let absent: marked_path::MarkedPathBuf<marked_path::Absolute> = {
            dir.path.join_relative(
                &marked_path::MarkedPathBuf::<marked_path::Relative>::new("absent.luau")
                    .expect("relative name")
                    .as_marked_path(),
            )
        };

        // When checking it.
        let report = check(&[absent]).expect_err("read error");

        // Then the read error names the path.
        assert!(report.current_context().sentence().contains("absent.luau"));
    }

    #[rstest::rstest]
    #[test]
    fn check_executes_nothing() {
        // Given a script whose side effect would create a file.
        install_crypto_provider();
        let dir = jinn_testutil::tempdir();
        let side_effect: marked_path::MarkedPathBuf<marked_path::Absolute> = {
            dir.path.join_relative(
                &marked_path::MarkedPathBuf::<marked_path::Relative>::new("side.luau")
                    .expect("relative name")
                    .as_marked_path(),
            )
        };
        let marker = dir.path.join("marker.txt").expect("joins").into_inner();
        std::fs::write(
            &side_effect,
            format!("local f = io.open({marker:?}, 'w') f:write('x') f:close()"),
        )
        .expect("write");

        // When checking it.
        let outcome = check(&[side_effect]).expect("checks");

        // Then the compile succeeds and no file was created (io is absent
        // in any case, but the point is check never ran the code).
        assert_eq!(outcome.status, ExitStatus(0));
        assert!(!marker.exists());
    }

    #[rstest::rstest]
    #[test]
    fn version_names_jinn_and_the_vendored_luau() {
        // Given the crate version.
        // When rendering the version line.
        let outcome = version(env!("CARGO_PKG_VERSION"));

        // Then it names both jinn and Luau.
        let line = outcome.lines.first().expect("one version line");
        assert!(line.contains("jinn "), "got {line}");
        assert!(line.contains("Luau"), "got {line}");
    }

    #[rstest::rstest]
    #[test]
    fn require_resolves_a_sibling_module_from_the_script_directory() {
        // Given a script and a sibling module in the same directory.
        install_crypto_provider();
        let dir = jinn_testutil::tempdir();
        let module = dir.path.join("helper.luau").expect("joins").into_inner();
        let script: marked_path::MarkedPathBuf<marked_path::Absolute> = {
            dir.path.join_relative(
                &marked_path::MarkedPathBuf::<marked_path::Relative>::new("main.luau")
                    .expect("relative name")
                    .as_marked_path(),
            )
        };
        std::fs::write(
            &module,
            "return { greet = function() return 'hi from helper' end }",
        )
        .expect("write module");
        std::fs::write(
            &script,
            "local h = require('./helper') return h.greet() == 'hi from helper' and 0 or 1",
        )
        .expect("write script");

        // When running the script.
        let outcome = run(
            &RunMode::Script(script.clone()),
            Vec::new(),
            Timeout::Default,
        )
        .expect("runs");

        // Then the require resolved and the script exited zero.
        assert_eq!(outcome.status, ExitStatus(0));
        let _ = module;
    }
}
