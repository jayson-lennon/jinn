//! Integration tests for `jinn luau` — the config-free contract.
//!
//! Runs the real binary against an isolated path root (every jinn base
//! directory rebound under one temp directory via `JINN_PATHS_ROOT`) with
//! **no config, no session store, and no running instance**, and asserts
//! the subcommand family works:
//!
//! - `jinn luau version` prints the jinn and vendored Luau versions.
//! - `jinn luau run` executes a script, passes post-`--` argv through
//!   `jinn.args`, and lands the script's exit code on the process.
//! - `jinn luau check` accepts a valid script (exit 0) and rejects a
//!   broken one with a line-positioned diagnostic (exit non-zero).
//! - `jinn luau --eval` evaluates one expression and prints its result.
//!
//! The `XDG_*` variables stay set for Linux parity, mirroring
//! `tests/install_cli.rs`: `dirs` reads them there and `JINN_PATHS_ROOT`
//! wins over both, so one code path covers every platform.
#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "integration test: fixture failures are the test's failure channel; CARGO_BIN_EXE and std::fs plumbing are the test harness itself"
)]

use marked_path::MarkedPathAccess as _;

/// Joins a relative script name onto the typed work path.
fn join_work(
    work: &marked_path::MarkedPathBuf<marked_path::Absolute>,
    name: &str,
) -> marked_path::MarkedPathBuf<marked_path::Absolute> {
    work.join_relative(
        &marked_path::MarkedPathBuf::<marked_path::Relative>::new(name)
            .expect("relative script name")
            .as_marked_path(),
    )
}

/// Resolves the built `jinn` binary for integration tests.
fn jinn_bin() -> std::path::PathBuf {
    std::env::var_os("CARGO_BIN_EXE_jinn")
        .map(std::path::PathBuf::from)
        .expect("CARGO_BIN_EXE_jinn not set — integration test must run via cargo test")
}

/// Runs `jinn luau …` with every path base rebound under a temp root and
/// the process cwd pinned to `work` (where the test writes its scripts).
fn run_luau(bin: &std::path::Path, work: &std::path::Path, args: &[&str]) -> std::process::Output {
    use marked_path::MarkedPathAccess as _;

    let dir = jinn_testutil::tempdir();
    let root = dir.path.as_path();
    let config = root.join("config");
    let data = root.join("data");
    std::fs::create_dir_all(&config).expect("create config dir");
    std::fs::create_dir_all(&data).expect("create data dir");

    let mut command = std::process::Command::new(bin);
    command
        .env("JINN_PATHS_ROOT", root)
        .env("XDG_CONFIG_HOME", config.as_path())
        .env("XDG_DATA_HOME", data.as_path())
        .current_dir(work)
        .args(args);
    command.output().expect("run jinn luau")
}

/// A temp directory standing in for the caller's project cwd.
///
/// Returns the owned temp root (kept alive for the test's duration) and the
/// typed absolute work path.
fn work_dir() -> (
    jinn_testutil::JinnTempDir,
    marked_path::MarkedPathBuf<marked_path::Absolute>,
) {
    use marked_path::MarkedPathAccess as _;
    let dir = jinn_testutil::tempdir();
    let work = dir.path.join_relative(
        &marked_path::MarkedPathBuf::<marked_path::Relative>::new("work")
            .expect("relative name")
            .as_marked_path(),
    );
    std::fs::create_dir_all(work.as_path()).expect("create work dir");
    (dir, work)
}

#[rstest::rstest]
#[test]
fn version_prints_jinn_and_luau_with_no_config_present() {
    // Given the real binary and a path root carrying no jinn.toml.
    let (_guard, work) = work_dir();
    let bin = jinn_bin();

    // When running `jinn luau version`.
    let output = run_luau(bin.as_path(), work.as_path(), &["luau", "version"]);

    // Then it succeeds and names both jinn and Luau.
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("jinn "), "got {stdout}");
    assert!(stdout.contains("Luau"), "got {stdout}");
}

#[rstest::rstest]
#[test]
fn run_executes_a_script_and_lands_its_exit_code() {
    // Given a script that exits 3 via an integer chunk return.
    let (guard, work) = work_dir();
    std::fs::write(join_work(&work, "code3.luau"), "return 3").expect("write script");
    let bin = jinn_bin();

    // When running it.
    let output = run_luau(
        bin.as_path(),
        work.as_path(),
        &["luau", "run", "code3.luau"],
    );

    // Then the process exits with the script's code.
    let code = output.status.code().expect("exited with a code");
    assert_eq!(
        code,
        3,
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    drop(guard);
}

#[rstest::rstest]
#[test]
fn run_passes_post_separator_argv_through_jinn_args() {
    // Given a script that succeeds only if jinn.args carries the probe.
    let (guard, work) = work_dir();
    std::fs::write(
        join_work(&work, "args.luau"),
        "assert(jinn.args[1] == 'probe-arg', 'args missing') return 0",
    )
    .expect("write script");
    let bin = jinn_bin();

    // When running it with an argv entry after `--`.
    let output = run_luau(
        bin.as_path(),
        work.as_path(),
        &["luau", "run", "args.luau", "--", "probe-arg"],
    );

    // Then it exits zero.
    let code = output.status.code().expect("exited with a code");
    assert_eq!(
        code,
        0,
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    drop(guard);
}

#[rstest::rstest]
#[test]
fn run_of_a_runtime_error_exits_one_with_a_sentence() {
    // Given a script that raises at runtime.
    let (guard, work) = work_dir();
    std::fs::write(
        join_work(&work, "boom.luau"),
        "error('integration failure')",
    )
    .expect("write script");
    let bin = jinn_bin();

    // When running it.
    let output = run_luau(bin.as_path(), work.as_path(), &["luau", "run", "boom.luau"]);

    // Then the process exits 1 and stderr names the failure.
    let code = output.status.code().expect("exited with a code");
    assert_eq!(code, 1);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("integration failure"), "got {stderr}");
    drop(guard);
}

#[rstest::rstest]
#[test]
fn check_accepts_a_valid_script_and_rejects_a_broken_one() {
    // Given one valid script and one with a syntax error.
    let (guard, work) = work_dir();
    std::fs::write(join_work(&work, "good.luau"), "return 1").expect("write good");
    std::fs::write(join_work(&work, "bad.luau"), "local x = = 1").expect("write bad");
    let bin = jinn_bin();

    // When checking the valid one.
    let good = run_luau(
        bin.as_path(),
        work.as_path(),
        &["luau", "check", "good.luau"],
    );

    // Then it exits zero.
    assert!(
        good.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&good.stderr)
    );

    // When checking the broken one.
    let bad = run_luau(
        bin.as_path(),
        work.as_path(),
        &["luau", "check", "bad.luau"],
    );

    // Then it exits non-zero with a positioned diagnostic.
    assert!(!bad.status.success());
    let stdout = String::from_utf8_lossy(&bad.stdout);
    assert!(stdout.contains("bad.luau"), "got {stdout}");
    assert!(stdout.contains(":1:"), "got {stdout}");
    drop(guard);
}

#[rstest::rstest]
#[test]
fn eval_prints_the_expression_result() {
    // Given the real binary and no config.
    let (_guard, work) = work_dir();
    let bin = jinn_bin();

    // When evaluating an expression (tostring so the chunk returns a
    // string — an integer return would map to that exit code instead).
    let output = run_luau(
        bin.as_path(),
        work.as_path(),
        &["luau", "--eval", "return tostring(41 + 1)"],
    );

    // Then the result prints and the process succeeds.
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("42"), "got {stdout}");
}

#[rstest::rstest]
#[test]
fn a_runaway_loop_dies_at_the_timeout_with_the_gnu_code() {
    // Given an infinite-loop script and a one-second budget.
    let (guard, work) = work_dir();
    std::fs::write(join_work(&work, "spin.luau"), "while true do end").expect("write script");
    let bin = jinn_bin();

    // When running it under --timeout 1.
    let output = run_luau(
        bin.as_path(),
        work.as_path(),
        &["luau", "--timeout", "1", "run", "spin.luau"],
    );

    // Then the process exits 124 (the GNU timeout convention).
    let code = output.status.code().expect("exited with a code");
    assert_eq!(
        code,
        124,
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    drop(guard);
}
