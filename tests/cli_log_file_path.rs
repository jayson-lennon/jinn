//! CLI-level integration tests for relative and tilde `--log-file` values.
//!
//! Spawns the real binary from a temp cwd with every jinn path base rebound
//! under a temp root (`JINN_PATHS_ROOT`), passes a *relative* `--log-file`,
//! and asserts the log file lands in the spawn cwd — the resolution the
//! shared CLI path resolver promises at the process boundary.

use marked_path::MarkedPathAccess;

/// Resolves the built `jinn` binary for integration tests.
#[expect(
    clippy::disallowed_types,
    reason = "cargo's CARGO_BIN_EXE plumbing, consumed only as a std::process::Command program"
)]
fn jinn_bin() -> std::path::PathBuf {
    std::env::var_os("CARGO_BIN_EXE_jinn")
        .map(std::path::PathBuf::from)
        .expect("CARGO_BIN_EXE_jinn not set — integration test must run via cargo test")
}

#[rstest::rstest]
#[test]
fn relative_log_file_resolves_against_the_spawn_cwd() {
    // Given a jinn run rooted in a temp tree, spawned from a temp cwd.
    let bin = jinn_bin();
    let root = jinn_testutil::tempdir();
    let cwd = root.path.join("cwd").expect("temp root joins a bare name");
    std::fs::create_dir_all(cwd.as_path()).expect("create the spawn cwd");
    let db_path = root
        .path
        .join("test.db")
        .expect("temp root joins a bare name");

    // When running `jinn --log-file foo.log completions bash` from that cwd.
    let output = std::process::Command::new(&bin)
        .env("JINN_PATHS_ROOT", root.path.as_path())
        .arg("--db-path")
        .arg(db_path.as_path())
        .arg("--log-file")
        .arg("foo.log")
        .arg("completions")
        .arg("bash")
        .current_dir(cwd.as_path())
        .output()
        .expect("run jinn");

    // Then the run succeeds.
    assert!(
        output.status.success(),
        "jinn failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // And the log file was created in the spawn cwd, not the state dir.
    let log = cwd.join("foo.log").expect("cwd joins a bare filename");
    assert!(
        log.as_path().exists(),
        "expected the log at {}",
        log.as_path().display()
    );
}
