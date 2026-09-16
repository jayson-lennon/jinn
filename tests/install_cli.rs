//! CLI-level integration tests for `jinn install`.
//!
//! Runs the real `install` dispatch against a temp XDG environment and
//! asserts the on-disk result: resource files, plugin payloads, and the
//! write-once `jinn.toml` semantics (created only when absent; an existing
//! file is never modified, even with `--force`).

use std::path::PathBuf;

/// Resolves the built `jinn` binary for integration tests.
fn jinn_bin() -> PathBuf {
    std::env::var_os("CARGO_BIN_EXE_jinn")
        .map(PathBuf::from)
        .expect("CARGO_BIN_EXE_jinn not set — integration test must run via cargo test")
}

/// Runs `jinn <args...>` inside the temp XDG environment.
fn run_jinn(
    bin: &std::path::Path,
    config: &std::path::Path,
    data: &std::path::Path,
    args: &[&str],
) -> std::process::Output {
    std::process::Command::new(bin)
        .env("XDG_CONFIG_HOME", config)
        .env("XDG_DATA_HOME", data)
        .arg("--db-path")
        .arg(data.join("unused.db"))
        .args(args)
        .output()
        .expect("run jinn")
}

// Given no existing jinn state in the temp environment.
// When running `jinn install`.
// Then it succeeds, seeds the plugin payloads, registers the first-party
// plugins, lists jinn.toml as Created, and prints the restart hint.
#[rstest::rstest]
#[test]
fn install_seeds_plugins_and_registers_entries() {
    let bin = jinn_bin();
    let (home, _guard) = temp_env();
    let config = home.join("config");
    let data = home.join("data");

    let output = run_jinn(&bin, &config, &data, &["install"]);
    assert!(
        output.status.success(),
        "jinn install failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("url-citations.wasm"));
    assert!(
        stdout.contains("Created") && stdout.contains("jinn.toml"),
        "first install must list jinn.toml as Created: {stdout}"
    );
    assert!(stdout.contains("Restart jinn to activate plugins."));
}

// Given a completed first install.
// When running `jinn install` again without --force.
// Then jinn.toml is listed as skipped alongside the other resources.
#[rstest::rstest]
#[test]
fn install_second_run_lists_jinn_toml_as_skipped() {
    let bin = jinn_bin();
    let (home, _guard) = temp_env();
    let config = home.join("config");
    let data = home.join("data");

    let first = run_jinn(&bin, &config, &data, &["install"]);
    assert!(first.status.success());

    let second = run_jinn(&bin, &config, &data, &["install"]);
    assert!(second.status.success());
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

// Given a completed first install whose jinn.toml the user hand-edited
// (disabled plugin + a comment).
// When running `jinn install --force`.
// Then jinn.toml is byte-identical while payloads are overwritten.
#[rstest::rstest]
#[test]
fn install_force_preserves_edited_jinn_toml() {
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
            "[plugin.url-citations]\nwasm = \"url-citations.wasm\"",
            "[plugin.url-citations]\nwasm = \"url-citations.wasm\"\nenabled = false",
        )
    );
    std::fs::write(&toml_path, &edited).expect("write edited jinn.toml");

    let forced = run_jinn(&bin, &config, &data, &["install", "--force"]);
    assert!(forced.status.success());
    let on_disk = std::fs::read_to_string(&toml_path).expect("read jinn.toml after --force");
    assert_eq!(on_disk, edited, "--force must never modify jinn.toml");
    // And the user's disabled flag survived (the edit landed in the file).
    assert!(on_disk.contains("enabled = false"));
    // And payloads still followed --force.
    let stdout = String::from_utf8_lossy(&forced.stdout);
    assert!(stdout.contains("Overwrote"));
}

// Given a malformed jinn.toml from a prior broken edit.
// When running `jinn install`.
// Then it succeeds and the file is left untouched.
#[rstest::rstest]
#[test]
fn install_succeeds_with_malformed_jinn_toml() {
    let bin = jinn_bin();
    let (home, _guard) = temp_env();
    let config = home.join("config");
    let data = home.join("data");

    let toml_dir = config.join("jinn");
    std::fs::create_dir_all(&toml_dir).expect("create config dir");
    std::fs::write(toml_dir.join("jinn.toml"), "NOT [valid toml").expect("write jinn.toml");

    let output = run_jinn(&bin, &config, &data, &["install"]);
    assert!(
        output.status.success(),
        "malformed jinn.toml must not fail install: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let on_disk = std::fs::read_to_string(toml_dir.join("jinn.toml")).expect("read jinn.toml");
    assert_eq!(
        on_disk, "NOT [valid toml",
        "install must not touch the file"
    );
}

// Given a jinn.toml missing one plugin entry and that payload deleted.
// When running `jinn install`.
// Then the payload is restored but NOT registered, and the output points at
// `jinn plugin install-builtins`.
#[rstest::rstest]
#[test]
fn install_notes_unregistered_payload_when_toml_exists() {
    let bin = jinn_bin();
    let (home, _guard) = temp_env();
    let config = home.join("config");
    let data = home.join("data");

    let first = run_jinn(&bin, &config, &data, &["install"]);
    assert!(first.status.success());

    // Drop the [plugin.url-citations] section and delete its payload.
    let toml_path = config.join("jinn/jinn.toml");
    let original = std::fs::read_to_string(&toml_path).expect("read jinn.toml");
    let edited = {
        let mut keeping = true;
        original
            .lines()
            .filter(|line| {
                if line.starts_with("[plugin.url-citations]") {
                    keeping = false;
                    return false;
                }
                if !keeping && line.starts_with('[') {
                    keeping = true;
                }
                keeping
            })
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
    };
    std::fs::write(&toml_path, &edited).expect("write edited jinn.toml");
    std::fs::remove_file(data.join("jinn/plugins/url-citations.wasm")).expect("remove payload");

    let output = run_jinn(&bin, &config, &data, &["install"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("install-builtins"),
        "output must point at `jinn plugin install-builtins`: {stdout}"
    );
    // And the missing entry was NOT re-added (add-only gap stays visible).
    let on_disk = std::fs::read_to_string(&toml_path).expect("read jinn.toml");
    assert!(
        !on_disk.contains("[plugin.url-citations]"),
        "install must not add entries to an existing jinn.toml"
    );
    // And the payload was restored.
    assert!(data.join("jinn/plugins/url-citations.wasm").is_file());
}

// A temp HOME whose subdirectories carry the XDG roots; the guard keeps the
// temp dir alive for the duration of the test.
fn temp_env() -> (PathBuf, tempfile::TempDir) {
    let dir = tempfile::TempDir::new().expect("temp dir");
    (dir.path().to_path_buf(), dir)
}

// ── `jinn plugin install-builtins` ──────────────────────────────────────────

// Given an empty environment.
// When running `jinn plugin install-builtins`.
// Then all payloads are written, all entries registered, and the restart
// hint printed.
#[rstest::rstest]
#[test]
fn install_builtins_fresh_env_registers_all() {
    let bin = jinn_bin();
    let (home, _guard) = temp_env();
    let config = home.join("config");
    let data = home.join("data");

    let output = run_jinn(&bin, &config, &data, &["plugin", "install-builtins"]);
    assert!(
        output.status.success(),
        "install-builtins failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Registered url-citations"));
    assert!(stdout.contains("Registered stall-watchdog"));
    assert!(stdout.contains("Restart jinn to activate plugins."));

    let toml = std::fs::read_to_string(config.join("jinn/jinn.toml")).expect("read jinn.toml");
    for name in [
        "url-citations",
        "url-citations",
        "tool-call-watchdog",
        "stall-watchdog",
    ] {
        assert!(
            toml.contains(&format!("[plugin.{name}]")),
            "{name} registered"
        );
    }
    assert!(data.join("jinn/plugins/url-citations.wasm").is_file());
}

// Given a completed first run whose jinn.toml the user hand-edited
// (disabled plugin + a comment).
// When running `jinn plugin install-builtins` again.
// Then jinn.toml is byte-identical (add-only registration) while payloads
// are refreshed.
#[rstest::rstest]
#[test]
fn install_builtins_preserves_existing_entries() {
    let bin = jinn_bin();
    let (home, _guard) = temp_env();
    let config = home.join("config");
    let data = home.join("data");

    let first = run_jinn(&bin, &config, &data, &["plugin", "install-builtins"]);
    assert!(first.status.success());

    let toml_path = config.join("jinn/jinn.toml");
    let original = std::fs::read_to_string(&toml_path).expect("read jinn.toml");
    let edited = format!(
        "# hands off\n{}",
        original.replace(
            "[plugin.url-citations]\nwasm = \"url-citations.wasm\"",
            "[plugin.url-citations]\nwasm = \"url-citations.wasm\"\nenabled = false",
        )
    );
    std::fs::write(&toml_path, &edited).expect("write edited jinn.toml");

    let second = run_jinn(&bin, &config, &data, &["plugin", "install-builtins"]);
    assert!(second.status.success());

    let on_disk = std::fs::read_to_string(&toml_path).expect("read jinn.toml");
    assert_eq!(on_disk, edited, "existing entries must be preserved");
    assert!(on_disk.contains("enabled = false"));

    let stdout = String::from_utf8_lossy(&second.stdout);
    assert!(
        stdout.contains("Already registered, skipped url-citations"),
        "existing entries must be skipped, got: {stdout}"
    );
}

// Given a jinn.toml missing exactly one plugin entry (payload still present).
// When running `jinn plugin install-builtins`.
// Then only that entry is added and the other entries stay byte-identical.
#[rstest::rstest]
#[test]
fn install_builtins_fills_only_missing_entry() {
    let bin = jinn_bin();
    let (home, _guard) = temp_env();
    let config = home.join("config");
    let data = home.join("data");

    let first = run_jinn(&bin, &config, &data, &["plugin", "install-builtins"]);
    assert!(first.status.success());

    // Drop the [plugin.stall-watchdog] section (keep its payload).
    let toml_path = config.join("jinn/jinn.toml");
    let original = std::fs::read_to_string(&toml_path).expect("read jinn.toml");
    let edited = {
        let mut keeping = true;
        original
            .lines()
            .filter(|line| {
                if line.starts_with("[plugin.stall-watchdog]") {
                    keeping = false;
                    return false;
                }
                if !keeping && line.starts_with('[') {
                    keeping = true;
                }
                keeping
            })
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
    };
    std::fs::write(&toml_path, &edited).expect("write edited jinn.toml");

    let second = run_jinn(&bin, &config, &data, &["plugin", "install-builtins"]);
    assert!(second.status.success());

    let stdout = String::from_utf8_lossy(&second.stdout);
    assert!(
        stdout.contains("Registered stall-watchdog"),
        "the missing entry must be registered, got: {stdout}"
    );
    assert!(
        stdout.contains("Already registered, skipped url-citations"),
        "existing entries must be skipped, got: {stdout}"
    );
    // And the other entries are unchanged.
    let on_disk = std::fs::read_to_string(&toml_path).expect("read jinn.toml");
    assert!(on_disk.contains("[plugin.stall-watchdog]"));
    assert!(on_disk.contains("[plugin.url-citations]"));
}

// Given a malformed jinn.toml.
// When running `jinn plugin install-builtins`.
// Then it fails fast: non-zero exit, a parse error mentioning the file, and
// zero payloads written.
#[rstest::rstest]
#[test]
fn install_builtins_fails_fast_on_malformed_jinn_toml() {
    let bin = jinn_bin();
    let (home, _guard) = temp_env();
    let config = home.join("config");
    let data = home.join("data");

    let toml_dir = config.join("jinn");
    std::fs::create_dir_all(&toml_dir).expect("create config dir");
    std::fs::write(toml_dir.join("jinn.toml"), "NOT [valid toml").expect("write jinn.toml");

    let output = run_jinn(&bin, &config, &data, &["plugin", "install-builtins"]);
    assert!(
        !output.status.success(),
        "malformed jinn.toml must fail install-builtins"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("jinn.toml"),
        "error must name the config file: {stderr}"
    );
    // And no payload side effects happened.
    let plugins_dir = data.join("jinn/plugins");
    assert!(
        !plugins_dir.exists()
            || std::fs::read_dir(&plugins_dir)
                .expect("readdir")
                .next()
                .is_none(),
        "fail-fast must leave the plugins dir empty"
    );
}
