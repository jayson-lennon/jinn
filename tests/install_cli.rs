//! CLI-level integration tests for `jinn install` and `jinn config init`.
//!
//! Runs the real binary against an isolated path root (every jinn base
//! directory — config, data, cache, state, home — rebinding under one temp
//! directory via `JINN_PATHS_ROOT`) and asserts the on-disk result:
//! resource files and the write-once `jinn.toml` and `stream_rules.toml`
//! semantics (created only when absent for install; an existing file is
//! never modified, even with `--force` — `config init --force` is the
//! overwrite path for both).
//!
//! The `XDG_*` variables stay set for Linux parity: there the `dirs` crate
//! reads them directly, and `JINN_PATHS_ROOT` wins over both, so one code
//! path covers every platform. On Windows `dirs` ignores `XDG_*` entirely,
//! which is exactly why the root override exists.

use std::path::PathBuf;

/// Resolves the built `jinn` binary for integration tests.
fn jinn_bin() -> PathBuf {
    std::env::var_os("CARGO_BIN_EXE_jinn")
        .map(PathBuf::from)
        .expect("CARGO_BIN_EXE_jinn not set — integration test must run via cargo test")
}

/// Runs `jinn <args...>` with every path base rebound under the temp root.
///
/// `JINN_PATHS_ROOT` rebinds config/data/cache/state/home under `root` (the
/// `AppPaths` override seam), so install/config-init targets, the log file,
/// and the skills directory all land inside the temp tree. The XDG variables
/// are set to the same root's subdirectories for platforms where `dirs`
/// honors them; the root override takes precedence either way.
fn run_jinn(
    bin: &std::path::Path,
    config: &std::path::Path,
    data: &std::path::Path,
    args: &[&str],
) -> std::process::Output {
    let root = config
        .parent()
        .expect("config dir always has a parent temp root");
    std::process::Command::new(bin)
        .env("JINN_PATHS_ROOT", root)
        .env("XDG_CONFIG_HOME", config)
        .env("XDG_DATA_HOME", data)
        .arg("--db-path")
        .arg(data.join("unused.db"))
        .args(args)
        .output()
        .expect("run jinn")
}

#[rstest::rstest]
#[test]
fn install_seeds_all_resource_kinds_and_creates_jinn_toml() {
    // Given no existing jinn state in the temp environment.
    let bin = jinn_bin();
    let (home, _guard) = temp_env();
    let config = home.join("config");
    let data = home.join("data");

    // When running `jinn install`.
    let output = run_jinn(&bin, &config, &data, &["install"]);

    // Then it succeeds.
    assert!(
        output.status.success(),
        "jinn install failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // And it seeds the four resource kinds (themes, personas, prompts, skills).
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("default.toml"), "theme seeded: {stdout}");
    assert!(stdout.contains("general.md"), "persona seeded: {stdout}");
    assert!(stdout.contains("plan.md"), "prompt seeded: {stdout}");
    assert!(stdout.contains("SKILL.md"), "skills seeded: {stdout}");
    // And it lists jinn.toml as Created.
    assert!(
        stdout.contains("Created") && stdout.contains("jinn.toml"),
        "first install must list jinn.toml as Created: {stdout}"
    );
    // And no plugin artifacts or hints remain anywhere in the install output.
    assert!(!stdout.contains("plugin"), "no plugin output: {stdout}");
    assert!(!stdout.contains(".wasm"), "no wasm payloads: {stdout}");
}

#[rstest::rstest]
#[test]
fn install_second_run_lists_jinn_toml_as_skipped() {
    // Given a completed first install.
    let bin = jinn_bin();
    let (home, _guard) = temp_env();
    let config = home.join("config");
    let data = home.join("data");

    let first = run_jinn(&bin, &config, &data, &["install"]);
    assert!(first.status.success());

    // When running `jinn install` again without --force.
    let second = run_jinn(&bin, &config, &data, &["install"]);

    // Then it succeeds.
    assert!(second.status.success());
    // And jinn.toml is listed as skipped alongside the other resources.
    let stdout = String::from_utf8_lossy(&second.stdout);
    let toml_line = stdout
        .lines()
        .find(|l| l.contains("jinn.toml"))
        .expect("output must mention jinn.toml");
    assert!(
        toml_line.starts_with("Already present, skipped"),
        "second run must list jinn.toml as skipped, got: {toml_line}"
    );
}

#[rstest::rstest]
#[test]
fn install_force_preserves_edited_jinn_toml() {
    // Given a completed first install whose jinn.toml the user hand-edited
    // (a compaction knob + a comment).
    let bin = jinn_bin();
    let (home, _guard) = temp_env();
    let config = home.join("config");
    let data = home.join("data");

    let first = run_jinn(&bin, &config, &data, &["install"]);
    assert!(first.status.success());

    let toml_path = config.join("jinn/jinn.toml");
    let original = std::fs::read_to_string(&toml_path).expect("read jinn.toml");
    let edited = format!(
        "# user was here\n{}",
        original.replace(
            "[context_curation.compaction]",
            "[context_curation.compaction]\nthreshold = 0.4",
        )
    );
    std::fs::write(&toml_path, &edited).expect("write edited jinn.toml");

    // When running `jinn install --force`.
    let forced = run_jinn(&bin, &config, &data, &["install", "--force"]);

    // Then it succeeds and jinn.toml is byte-identical.
    assert!(forced.status.success());
    let on_disk = std::fs::read_to_string(&toml_path).expect("read jinn.toml after --force");
    assert_eq!(on_disk, edited, "--force must never modify jinn.toml");
    // And the user's knob survived (the edit landed in the file).
    assert!(on_disk.contains("threshold = 0.4"));
    // And resource files still followed --force.
    let stdout = String::from_utf8_lossy(&forced.stdout);
    assert!(stdout.contains("Overwrote"));
}

#[rstest::rstest]
#[test]
fn install_succeeds_with_malformed_jinn_toml() {
    // Given a malformed jinn.toml from a prior broken edit.
    let bin = jinn_bin();
    let (home, _guard) = temp_env();
    let config = home.join("config");
    let data = home.join("data");

    let toml_dir = config.join("jinn");
    std::fs::create_dir_all(&toml_dir).expect("create config dir");
    std::fs::write(toml_dir.join("jinn.toml"), "NOT [valid toml").expect("write jinn.toml");

    // When running `jinn install`.
    let output = run_jinn(&bin, &config, &data, &["install"]);

    // Then it succeeds.
    assert!(
        output.status.success(),
        "malformed jinn.toml must not fail install: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // And the malformed file is left untouched.
    let on_disk = std::fs::read_to_string(toml_dir.join("jinn.toml")).expect("read jinn.toml");
    assert_eq!(
        on_disk, "NOT [valid toml",
        "install must not touch the file"
    );
}

#[rstest::rstest]
#[test]
fn config_init_creates_the_stream_rules_file_beside_jinn_toml() {
    // Given a fresh temp environment with no config at all.
    let bin = jinn_bin();
    let (home, _guard) = temp_env();
    let config = home.join("config");
    let data = home.join("data");

    // When running `jinn config init`.
    let output = run_jinn(&bin, &config, &data, &["config", "init"]);

    // Then it succeeds and reports both files created.
    assert!(
        output.status.success(),
        "config init failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let rules_line = stdout
        .lines()
        .find(|l| l.contains("stream_rules.toml"))
        .expect("config init must report the rules file");
    assert!(
        rules_line.starts_with("Created"),
        "fresh init must create the rules file, got: {rules_line}"
    );
    // And the rules file exists beside jinn.toml with the embedded template.
    let rules_path = config.join("jinn/stream_rules.toml");
    assert!(rules_path.is_file(), "rules file must be beside jinn.toml");
    assert_eq!(
        std::fs::read_to_string(&rules_path).expect("read rules file"),
        jinn_preferences_config::STREAM_RULES_TEMPLATE,
        "the created rules file is the embedded template, verbatim"
    );
}

#[rstest::rstest]
#[test]
fn config_init_force_overwrites_the_stream_rules_file() {
    // Given a completed init whose rules file the user replaced with edits.
    let bin = jinn_bin();
    let (home, _guard) = temp_env();
    let config = home.join("config");
    let data = home.join("data");

    let first = run_jinn(&bin, &config, &data, &["config", "init"]);
    assert!(first.status.success());
    let rules_path = config.join("jinn/stream_rules.toml");
    std::fs::write(&rules_path, "# only mine\n").expect("write edited rules");

    // When running `jinn config init --force`.
    let forced = run_jinn(&bin, &config, &data, &["config", "init", "--force"]);

    // Then it succeeds and the rules file is back to the shipped template:
    // --force is the supported path for pulling in an updated shipped set.
    assert!(forced.status.success());
    let stdout = String::from_utf8_lossy(&forced.stdout);
    let rules_line = stdout
        .lines()
        .find(|l| l.contains("stream_rules.toml"))
        .expect("forced init must report the rules file");
    assert!(
        rules_line.starts_with("Overwrote"),
        "--force must overwrite the rules file, got: {rules_line}"
    );
    assert_eq!(
        std::fs::read_to_string(&rules_path).expect("read rules file"),
        jinn_preferences_config::STREAM_RULES_TEMPLATE
    );
}

#[rstest::rstest]
#[test]
fn config_init_without_force_skips_an_existing_stream_rules_file() {
    // Given a rules file already present (e.g. from `jinn install`) but no
    // jinn.toml beside it.
    let bin = jinn_bin();
    let (home, _guard) = temp_env();
    let config = home.join("config");
    let data = home.join("data");
    let toml_dir = config.join("jinn");
    std::fs::create_dir_all(&toml_dir).expect("create config dir");
    std::fs::write(toml_dir.join("stream_rules.toml"), "# only mine\n").expect("write rules");

    // When running `jinn config init` without --force.
    let output = run_jinn(&bin, &config, &data, &["config", "init"]);

    // Then it succeeds — jinn.toml is created — and the rules file is
    // listed as skipped, content intact.
    assert!(
        output.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let rules_line = stdout
        .lines()
        .find(|l| l.contains("stream_rules.toml"))
        .expect("init must mention the rules file");
    assert!(
        rules_line.starts_with("Already present, skipped"),
        "plain init must not touch an existing rules file, got: {rules_line}"
    );
    assert_eq!(
        std::fs::read_to_string(toml_dir.join("stream_rules.toml")).expect("read rules file"),
        "# only mine\n"
    );
}

// A temp root whose `config/` and `data/` subdirectories carry the XDG
// locations; the guard keeps the temp dir alive for the duration of the
// test. `run_jinn` points `JINN_PATHS_ROOT` at the root itself, so every
// jinn path base — including home, and therefore `.agents/skills` —
// rebinds inside this tree and the run touches nothing real.
fn temp_env() -> (PathBuf, tempfile::TempDir) {
    let dir = tempfile::TempDir::new().expect("temp dir");
    (dir.path().to_path_buf(), dir)
}
