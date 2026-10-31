//! CLI-level integration tests for `jinn install` and `jinn config init`.
//!
//! Runs the real binary against an isolated path root (every jinn base
//! directory — config, data, cache, state, home — rebinding under one temp
//! directory via `JINN_PATHS_ROOT`) and asserts the on-disk result:
//!
//! - `jinn install` seeds the three config files write-once (`jinn.toml`,
//!   `providers.toml`, `stream_rules.toml`); an existing file is never
//!   modified, and there is no `--force` anymore.
//! - `jinn install --builtins` force-restores the bundled resources over
//!   user edits and touches no config file.
//! - `jinn config init --force` remains the overwrite path for the config
//!   documents.
//!
//! The `XDG_*` variables stay set for Linux parity: there the `dirs` crate
//! reads them directly, and `JINN_PATHS_ROOT` wins over both, so one code
//! path covers every platform. On Windows `dirs` ignores `XDG_*` entirely,
//! which is exactly why the root override exists.

use marked_path::{Absolute, MarkedPathAccess, MarkedPathBuf, Relative};
use sha2::Digest;
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
/// the manifest, and the skills directory all land inside the temp tree.
/// The XDG variables are set to the same root's subdirectories for
/// platforms where `dirs` honors them; the root override takes precedence
/// either way.
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

/// A temp root whose `config/` and `data/` subdirectories carry the XDG
/// locations; the guard keeps the temp dir alive for the duration of the
/// test. `run_jinn` points `JINN_PATHS_ROOT` at the root itself, so every
/// jinn path base — including home, and therefore `.agents/skills` —
/// rebinds inside this tree and the run touches nothing real.
fn temp_env() -> (MarkedPathBuf<Absolute>, jinn_testutil::JinnTempDir) {
    let dir = jinn_testutil::tempdir();
    (dir.path.clone(), dir)
}

/// The standard derived paths inside a temp root.
struct Root {
    config: MarkedPathBuf<Absolute>,
    data: MarkedPathBuf<Absolute>,
}

impl Root {
    fn of(home: &MarkedPathBuf<Absolute>) -> Root {
        Root {
            config: home.join("config").expect("typed home joins a bare name"),
            data: home.join("data").expect("typed home joins a bare name"),
        }
    }
}

/// The `jinn.toml` path in a temp root's config dir.
fn jinn_toml_path(root: &Root) -> MarkedPathBuf<Absolute> {
    root.config
        .join_all(["jinn", "jinn.toml"])
        .expect("typed parent joins a bare name")
}

/// The `providers.toml` path in a temp root's config dir.
fn providers_toml_path(root: &Root) -> MarkedPathBuf<Absolute> {
    root.config
        .join_all(["jinn", "providers.toml"])
        .expect("typed parent joins a bare name")
}

/// The `stream_rules.toml` path in a temp root's config dir.
fn stream_rules_path(root: &Root) -> MarkedPathBuf<Absolute> {
    root.config
        .join_all(["jinn", "stream_rules.toml"])
        .expect("typed parent joins a bare name")
}

/// The bundled-files manifest path in a temp root's state dir.
fn manifest_path(home: &MarkedPathBuf<Absolute>) -> MarkedPathBuf<Absolute> {
    home.join_all(["state", "jinn", "bundled-files.toml"])
        .expect("typed parent joins a bare name")
}

/// The default theme's on-disk path in a temp root.
fn default_theme_path(root: &Root) -> MarkedPathBuf<Absolute> {
    root.config
        .join_all(["jinn", "themes", "default.toml"])
        .expect("typed parent joins a bare name")
}

/// Lowercase hex sha256 of `bytes` — the manifest's hash form.
fn sha256_hex(bytes: &[u8]) -> String {
    let digest = sha2::Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Spells a `res/`-relative key the way production does — through the typed
/// relative path, whose separators are the platform's own
/// (`themes/default.toml` on Unix, `themes\default.toml` on Windows). Every
/// manifest lookup and crafted entry goes through this, so a test can never
/// pin a key to one platform's spelling.
fn res_key(res_relative: &str) -> String {
    MarkedPathBuf::<Relative>::new(res_relative)
        .expect("res-relative key is a valid relative path")
        .as_path()
        .to_string_lossy()
        .into_owned()
}

/// The manifest as (path → sha256) pairs, parsed from disk.
fn manifest_entries(path: &MarkedPathBuf<Absolute>) -> std::collections::BTreeMap<String, String> {
    ManifestDoc::read(path).entries
}

/// The on-disk manifest as an editable document: the `[[file]]` entries as
/// (path → sha256), re-serialized whole on write. Crafting mutates the entry
/// map and serializes the result, so an edit either lands on its named
/// entry or fails the test — a text-level rewrite can silently miss.
struct ManifestDoc {
    entries: std::collections::BTreeMap<String, String>,
}

impl ManifestDoc {
    /// Parses the manifest at `path`.
    fn read(path: &MarkedPathBuf<Absolute>) -> Self {
        let text = std::fs::read_to_string(path.as_path()).expect("read manifest");
        let value: toml::Table = toml::from_str(&text).expect("manifest parses");
        assert_eq!(value.get("version"), Some(&toml::Value::from(1)));
        let files = value
            .get("file")
            .and_then(toml::Value::as_array)
            .expect("manifest carries a file array");
        let entries = files
            .iter()
            .map(|entry| {
                let entry_path = entry
                    .get("path")
                    .and_then(toml::Value::as_str)
                    .expect("entry names a path");
                let hash = entry
                    .get("sha256")
                    .and_then(toml::Value::as_str)
                    .expect("entry names a hash");
                (entry_path.to_owned(), hash.to_owned())
            })
            .collect();
        Self { entries }
    }

    /// Overwrites one tracked entry's hash; the entry must already exist.
    fn set_hash(&mut self, key: &str, hash: &str) {
        assert!(
            self.entries.contains_key(key),
            "crafted edit targets a manifest entry that does not exist: {key}"
        );
        self.entries.insert(key.to_owned(), hash.to_owned());
    }

    /// Adds a crafted entry; the key must not be tracked yet.
    fn insert(&mut self, key: &str, hash: &str) {
        let previous = self.entries.insert(key.to_owned(), hash.to_owned());
        assert!(previous.is_none(), "crafted entry already tracked: {key}");
    }

    /// Serializes the document back over the manifest at `path`.
    fn write(&self, path: &MarkedPathBuf<Absolute>) {
        let files: Vec<toml::Value> = self
            .entries
            .iter()
            .map(|(entry_path, hash)| {
                let mut entry = toml::Table::new();
                entry.insert("path".into(), toml::Value::from(entry_path.as_str()));
                entry.insert("sha256".into(), toml::Value::from(hash.as_str()));
                toml::Value::Table(entry)
            })
            .collect();
        let mut document = toml::Table::new();
        document.insert("version".into(), toml::Value::from(1));
        document.insert("file".into(), toml::Value::from(files));
        let serialized = toml::to_string(&document).expect("manifest serializes");
        std::fs::write(path.as_path(), serialized).expect("write manifest");
    }
}

#[rstest::rstest]
#[test]
fn install_seeds_only_the_three_config_files() {
    // Given no existing jinn state in the temp environment.
    let bin = jinn_bin();
    let (home, _guard) = temp_env();
    let root = Root::of(&home);

    // When running `jinn install`.
    let output = run_jinn(
        &bin,
        root.config.as_path(),
        root.data.as_path(),
        &["install"],
    );

    // Then it succeeds.
    assert!(
        output.status.success(),
        "jinn install failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // And all three config files were created.
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Created"), "reports creations: {stdout}");
    assert!(jinn_toml_path(&root).exists(), "jinn.toml must be created");
    assert!(
        providers_toml_path(&root).exists(),
        "providers.toml must be created"
    );
    assert!(
        stream_rules_path(&root).exists(),
        "stream_rules.toml must be created"
    );

    // And no resource lands outside the config files: themes stay empty —
    // resources are the reconciler's job, not install's.
    let themes_dir = root.config.join_all(["jinn", "themes"]).expect("joins");
    assert!(
        !themes_dir.exists() || std::fs::read_dir(themes_dir.as_path()).is_err_and(|_| true),
        "install must not seed themes"
    );

    // And no manifest was written: install seeds no bundled resources.
    assert!(
        !manifest_path(&home).exists(),
        "install must not write the bundled-files manifest"
    );
}

#[rstest::rstest]
#[test]
fn install_second_run_skips_every_config_file() {
    // Given a completed first install.
    let bin = jinn_bin();
    let (home, _guard) = temp_env();
    let root = Root::of(&home);

    let first = run_jinn(
        &bin,
        root.config.as_path(),
        root.data.as_path(),
        &["install"],
    );
    assert!(first.status.success());

    // When running `jinn install` again.
    let second = run_jinn(
        &bin,
        root.config.as_path(),
        root.data.as_path(),
        &["install"],
    );

    // Then it succeeds.
    assert!(second.status.success());
    // And every config file is listed as skipped, not an error.
    let stdout = String::from_utf8_lossy(&second.stdout);
    let skip_count = stdout.matches("Already present, skipped").count();
    assert_eq!(
        skip_count, 3,
        "all three config files must be skipped, got: {stdout}"
    );
}

#[rstest::rstest]
#[test]
fn install_never_touches_an_edited_jinn_toml() {
    // Given a completed first install whose jinn.toml the user hand-edited
    // (a comment + a knob).
    let bin = jinn_bin();
    let (home, _guard) = temp_env();
    let root = Root::of(&home);

    let first = run_jinn(
        &bin,
        root.config.as_path(),
        root.data.as_path(),
        &["install"],
    );
    assert!(first.status.success());

    let toml_path = jinn_toml_path(&root);
    let original = std::fs::read_to_string(toml_path.as_path()).expect("read jinn.toml");
    let edited = format!(
        "# user was here\n{}",
        original.replace(
            "[context_curation.compaction]",
            "[context_curation.compaction]\nthreshold = 0.4",
        )
    );
    std::fs::write(&toml_path, &edited).expect("write edited jinn.toml");

    // When running `jinn install` again.
    let second = run_jinn(
        &bin,
        root.config.as_path(),
        root.data.as_path(),
        &["install"],
    );

    // Then it succeeds and jinn.toml is byte-identical.
    assert!(second.status.success());
    let on_disk = std::fs::read_to_string(&toml_path).expect("read jinn.toml after install");
    assert_eq!(on_disk, edited, "install must never modify jinn.toml");
    // And the user's knob survived.
    assert!(on_disk.contains("threshold = 0.4"));
}

#[rstest::rstest]
#[test]
fn install_rejects_force() {
    // Given a jinn binary with the new CLI surface.
    let bin = jinn_bin();
    let (home, _guard) = temp_env();
    let root = Root::of(&home);

    // When running `jinn install --force`.
    let output = run_jinn(
        &bin,
        root.config.as_path(),
        root.data.as_path(),
        &["install", "--force"],
    );

    // Then clap rejects the unknown flag.
    assert!(
        !output.status.success(),
        "--force must no longer exist on install"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unexpected argument"),
        "clap must report the unknown flag: {stderr}"
    );
}

#[rstest::rstest]
#[test]
fn install_succeeds_with_malformed_jinn_toml() {
    // Given a malformed jinn.toml from a prior broken edit.
    let bin = jinn_bin();
    let (home, _guard) = temp_env();
    let root = Root::of(&home);

    let toml_dir = root.config.join("jinn").expect("typed parent joins a name");
    std::fs::create_dir_all(&toml_dir).expect("create config dir");
    std::fs::write(jinn_toml_path(&root), "NOT [valid toml").expect("write jinn.toml");

    // When running `jinn install`.
    let output = run_jinn(
        &bin,
        root.config.as_path(),
        root.data.as_path(),
        &["install"],
    );

    // Then it succeeds.
    assert!(
        output.status.success(),
        "malformed jinn.toml must not fail install: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // And the malformed file is left untouched.
    let on_disk = std::fs::read_to_string(jinn_toml_path(&root)).expect("read jinn.toml");
    assert_eq!(
        on_disk, "NOT [valid toml",
        "install must not touch the file"
    );
}

#[rstest::rstest]
#[test]
fn boot_reconcile_writes_bundled_files_and_a_complete_manifest() {
    // Given a fresh temp environment and a launch that proceeds past the
    // recovery subcommands (`completions` prints and exits; prepare runs).
    let bin = jinn_bin();
    let (home, _guard) = temp_env();
    let root = Root::of(&home);

    let output = run_jinn(
        &bin,
        root.config.as_path(),
        root.data.as_path(),
        &["completions", "bash"],
    );
    assert!(
        output.status.success(),
        "launch failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Then the bundled theme was written to the user dir.
    let theme_path = default_theme_path(&root);
    assert!(theme_path.exists(), "boot reconcile must seed the theme");

    // And the manifest lists exactly it under the shipped hash: the entry
    // set is the written set (here both single-file, as every other bundled
    // file was user-absent… but this root is fresh, so the manifest carries
    // the whole shipped catalogue).
    let entries = manifest_entries(&manifest_path(&home));
    assert!(
        !entries.is_empty(),
        "manifest must list the reconciled files"
    );
    let theme_bytes = std::fs::read(theme_path.as_path()).expect("read written theme");
    assert_eq!(
        entries.get(&res_key("themes/default.toml")),
        Some(&sha256_hex(&theme_bytes)),
        "the manifest records the written theme under its hash"
    );
    // And every manifest entry names a file that now exists on disk: the
    // kind directory selects the destination root, and the remainder joins
    // beneath it typed, exactly as the installer resolves destinations.
    for key in entries.keys() {
        let (kind_dir, rest) = key
            .split_once('/')
            .expect("manifest keys carry a kind directory");
        let root = match kind_dir {
            "themes" | "personas" | "prompts" | "workflows" => root.config.join("jinn"),
            "skills" => home.join(".agents"),
            other => panic!("unexpected manifest entry kind: {other:?}"),
        }
        .expect("typed parent joins a bare name");
        let subdirs = match kind_dir {
            "skills" => ["skills"],
            _ => [kind_dir],
        };
        let disk = root
            .join_all(subdirs.into_iter().chain(std::iter::once(rest)))
            .expect("typed destination joins the entry remainder");
        assert!(disk.exists(), "manifest entry {key} names no written file");
    }
}

#[rstest::rstest]
#[test]
fn boot_reconcile_is_idempotent_across_runs() {
    // Given a first launch that reconciled a fresh root.
    let bin = jinn_bin();
    let (home, _guard) = temp_env();
    let root = Root::of(&home);

    let first = run_jinn(
        &bin,
        root.config.as_path(),
        root.data.as_path(),
        &["completions", "bash"],
    );
    assert!(first.status.success());

    let theme_bytes = std::fs::read(default_theme_path(&root).as_path()).expect("read theme");
    let manifest_before = std::fs::read(manifest_path(&home).as_path()).expect("read manifest");

    // When launching again.
    let second = run_jinn(
        &bin,
        root.config.as_path(),
        root.data.as_path(),
        &["completions", "bash"],
    );
    assert!(second.status.success());

    // Then disk and manifest are byte-identical.
    assert_eq!(
        std::fs::read(default_theme_path(&root).as_path()).expect("read theme"),
        theme_bytes,
        "second run must not rewrite the theme"
    );
    assert_eq!(
        std::fs::read(manifest_path(&home).as_path()).expect("read manifest"),
        manifest_before,
        "second run must not rewrite the manifest"
    );
}

#[rstest::rstest]
#[test]
fn boot_reconcile_keeps_a_user_edited_bundled_file() {
    // Given a reconciled root whose theme the user then edited.
    let bin = jinn_bin();
    let (home, _guard) = temp_env();
    let root = Root::of(&home);

    let setup = run_jinn(
        &bin,
        root.config.as_path(),
        root.data.as_path(),
        &["completions", "bash"],
    );
    assert!(setup.status.success());

    let theme_path = default_theme_path(&root);
    std::fs::write(theme_path.as_path(), "# the user's theme\n").expect("edit theme");

    // When launching again.
    let next = run_jinn(
        &bin,
        root.config.as_path(),
        root.data.as_path(),
        &["completions", "bash"],
    );
    assert!(next.status.success());

    // Then the user's bytes survive untouched.
    assert_eq!(
        std::fs::read_to_string(theme_path.as_path()).expect("read theme"),
        "# the user's theme\n",
        "boot reconcile must never overwrite a user edit"
    );
    // And the entry was dropped: the file is user-owned now.
    let entries = manifest_entries(&manifest_path(&home));
    assert!(
        !entries.contains_key(&res_key("themes/default.toml")),
        "the edited file must be untracked"
    );
}

#[rstest::rstest]
#[test]
fn boot_reconcile_heals_a_tracked_stale_file() {
    // Given a reconciled root, then a crafted state simulating an update:
    // the manifest claims a stale copy that is indeed on disk.
    let bin = jinn_bin();
    let (home, _guard) = temp_env();
    let root = Root::of(&home);

    let setup = run_jinn(
        &bin,
        root.config.as_path(),
        root.data.as_path(),
        &["completions", "bash"],
    );
    assert!(setup.status.success());

    let theme_path = default_theme_path(&root);
    std::fs::write(theme_path.as_path(), "stale bytes from an older binary")
        .expect("write stale theme");
    let stale_hash = sha256_hex(b"stale bytes from an older binary");
    let manifest_file = manifest_path(&home);
    {
        let mut doc = ManifestDoc::read(&manifest_file);
        doc.set_hash(&res_key("themes/default.toml"), &stale_hash);
        doc.write(&manifest_file);
    }

    // When launching again.
    let next = run_jinn(
        &bin,
        root.config.as_path(),
        root.data.as_path(),
        &["completions", "bash"],
    );
    assert!(next.status.success());

    // Then the stale copy was healed to the shipped bytes.
    let shipped = std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("res/themes/default.toml"),
    )
    .expect("read shipped theme from the repo tree");
    assert_eq!(
        std::fs::read(theme_path.as_path()).expect("read healed theme"),
        shipped,
        "the tracked stale file must be overwritten with the shipped bytes"
    );
    // And the manifest records the shipped hash again.
    let entries = manifest_entries(&manifest_file);
    assert_eq!(
        entries.get(&res_key("themes/default.toml")),
        Some(&sha256_hex(&shipped))
    );
}

#[rstest::rstest]
#[test]
fn boot_reconcile_deletes_an_owned_unshipped_file_and_keeps_an_edited_one() {
    // Given a reconciled root, then two crafted unshipped entries: one
    // whose file still matches what jinn wrote, one that was edited after.
    let bin = jinn_bin();
    let (home, _guard) = temp_env();
    let root = Root::of(&home);

    let setup = run_jinn(
        &bin,
        root.config.as_path(),
        root.data.as_path(),
        &["completions", "bash"],
    );
    assert!(setup.status.success());

    let gone_path = root
        .config
        .join_all(["jinn", "themes", "gone.toml"])
        .expect("joins");
    std::fs::write(gone_path.as_path(), "what jinn once wrote").expect("seed gone theme");
    let edited_path = root
        .config
        .join_all(["jinn", "themes", "custom-legacy.toml"])
        .expect("joins");
    std::fs::write(edited_path.as_path(), "# the user's legacy theme\n")
        .expect("seed edited legacy theme");

    let manifest_file = manifest_path(&home);
    {
        let mut doc = ManifestDoc::read(&manifest_file);
        doc.set_hash(&res_key("themes/default.toml"), "placeholder");
        doc.insert(
            &res_key("themes/gone.toml"),
            &sha256_hex(b"what jinn once wrote"),
        );
        doc.insert(
            &res_key("themes/custom-legacy.toml"),
            &sha256_hex(b"placeholder-not-what-is-on-disk"),
        );
        doc.write(&manifest_file);
    }

    // When launching again.
    let next = run_jinn(
        &bin,
        root.config.as_path(),
        root.data.as_path(),
        &["completions", "bash"],
    );
    assert!(next.status.success());

    // Then the owned unshipped file was deleted.
    assert!(
        !gone_path.exists(),
        "an owned unshipped file must be deleted"
    );
    // And the edited unshipped file was kept.
    assert_eq!(
        std::fs::read_to_string(edited_path.as_path()).expect("read kept file"),
        "# the user's legacy theme\n",
        "an edited unshipped file must be kept"
    );
    // And both entries are forgotten.
    let entries = manifest_entries(&manifest_file);
    assert!(!entries.contains_key(&res_key("themes/gone.toml")));
    assert!(!entries.contains_key(&res_key("themes/custom-legacy.toml")));
    // While the shipped theme is still tracked.
    assert!(entries.contains_key(&res_key("themes/default.toml")));
}

#[rstest::rstest]
#[test]
fn install_builtins_overwrites_user_edits_and_reclaims_them() {
    // Given a reconciled root whose theme the user edited.
    let bin = jinn_bin();
    let (home, _guard) = temp_env();
    let root = Root::of(&home);

    let setup = run_jinn(
        &bin,
        root.config.as_path(),
        root.data.as_path(),
        &["completions", "bash"],
    );
    assert!(setup.status.success());
    let theme_path = default_theme_path(&root);
    std::fs::write(theme_path.as_path(), "# the user's theme\n").expect("edit theme");
    // And a config file present with user content.
    let toml_path = jinn_toml_path(&root);
    let config_before = std::fs::read_to_string(toml_path.as_path()).expect("read jinn.toml");

    // When running `jinn install --builtins`.
    let output = run_jinn(
        &bin,
        root.config.as_path(),
        root.data.as_path(),
        &["install", "--builtins"],
    );
    assert!(
        output.status.success(),
        "install --builtins failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Then the theme was reclaimed with the shipped bytes.
    let shipped = std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("res/themes/default.toml"),
    )
    .expect("read shipped theme");
    assert_eq!(
        std::fs::read(theme_path.as_path()).expect("read reclaimed theme"),
        shipped,
        "--builtins must overwrite user edits"
    );
    // And the manifest tracks the file again.
    let entries = manifest_entries(&manifest_path(&home));
    assert_eq!(
        entries.get(&res_key("themes/default.toml")),
        Some(&sha256_hex(&shipped))
    );

    // And the config files were not touched.
    assert_eq!(
        std::fs::read_to_string(toml_path.as_path()).expect("read jinn.toml"),
        config_before,
        "--builtins must never modify jinn.toml"
    );
}

#[rstest::rstest]
#[test]
fn install_command_output_carries_no_reconcile_actions() {
    // Given a fresh root (an install here must not reconcile).
    let bin = jinn_bin();
    let (home, _guard) = temp_env();
    let root = Root::of(&home);

    // When running `jinn install`.
    let output = run_jinn(
        &bin,
        root.config.as_path(),
        root.data.as_path(),
        &["install"],
    );
    assert!(output.status.success());

    // Then no bundled resource file was written anywhere: no theme, no
    // personas, no skills.
    assert!(
        !default_theme_path(&root).exists(),
        "install must not seed resources as a side effect"
    );
    assert!(
        !home
            .join_all([".agents", "skills", "jinn-usage", "SKILL.md"])
            .expect("joins")
            .exists(),
        "install must not seed skills as a side effect"
    );
}

#[rstest::rstest]
#[test]
fn config_init_creates_the_stream_rules_file_beside_jinn_toml() {
    // Given a fresh temp environment with no config at all.
    let bin = jinn_bin();
    let (home, _guard) = temp_env();
    let root = Root::of(&home);

    // When running `jinn config init`.
    let output = run_jinn(
        &bin,
        root.config.as_path(),
        root.data.as_path(),
        &["config", "init"],
    );

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
    let rules_path = stream_rules_path(&root);
    assert!(
        rules_path.as_path().is_file(),
        "rules file must be beside jinn.toml"
    );
    assert_eq!(
        std::fs::read_to_string(rules_path.as_path()).expect("read rules file"),
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
    let root = Root::of(&home);

    let first = run_jinn(
        &bin,
        root.config.as_path(),
        root.data.as_path(),
        &["config", "init"],
    );
    assert!(first.status.success());
    let rules_path = stream_rules_path(&root);
    std::fs::write(rules_path.as_path(), "# only mine\n").expect("write edited rules");

    // When running `jinn config init --force`.
    let forced = run_jinn(
        &bin,
        root.config.as_path(),
        root.data.as_path(),
        &["config", "init", "--force"],
    );

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
        std::fs::read_to_string(rules_path.as_path()).expect("read rules file"),
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
    let root = Root::of(&home);
    let toml_dir = root.config.join("jinn").expect("typed parent joins a name");
    std::fs::create_dir_all(&toml_dir).expect("create config dir");
    std::fs::write(stream_rules_path(&root), "# only mine\n").expect("write rules");

    // When running `jinn config init` without --force.
    let output = run_jinn(
        &bin,
        root.config.as_path(),
        root.data.as_path(),
        &["config", "init"],
    );

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
        std::fs::read_to_string(stream_rules_path(&root)).expect("read rules file"),
        "# only mine\n"
    );
}
