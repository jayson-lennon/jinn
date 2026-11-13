//! Machine manifests: the on-disk TOML profile a smolvm machine boots from.
//!
//! A manifest is a small declarative profile — image, resources, network,
//! and a first-boot `init` list. jinn ships one (`alpine.toml`, seeded by
//! the reconciler into `~/.config/jinn/smolvm/`) and users write their own;
//! the server resolves one path per boot and reads the file from disk.
//!
//! The parse is strict (unknown keys refused) so a typo fails at boot with
//! the key named, matching the engine's own Smolfile convention.

use jinn_common::app_paths::AppPaths;
use marked_path::{Absolute, MarkedPath, MarkedPathAccess, MarkedPathBuf, PathError};
use serde::Deserialize;
use wherror::Error;

/// The manifest the server boots when no `--smolfile` override is given:
/// the reconciler-seeded Alpine profile.
pub const DEFAULT_MANIFEST_FILE: &str = "alpine.toml";

/// Error while resolving or parsing a machine manifest.
#[derive(Debug, Error)]
#[error(debug)]
pub(crate) struct ManifestError;

/// A machine manifest: the declarative profile one smolvm machine boots
/// from.
///
/// Every field is required except `init`; the engine itself defaults
/// everything it needs, but a jinn manifest names its machine on purpose —
/// a profile that says nothing is a typo waiting to boot the wrong thing.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineManifest {
    /// The OCI image the machine boots (e.g. `alpine:latest`).
    pub image: String,
    /// Virtual CPU count.
    pub cpus: u8,
    /// Guest memory, in MiB.
    pub memory: u32,
    /// Whether the guest gets network access. The `init` list needs this
    /// on to install packages.
    pub net: bool,
    /// Shell commands run once as root on the machine's first start,
    /// before tools are served. Absent for a bare machine.
    #[serde(default)]
    pub init: Vec<String>,
}

/// Resolves the manifest path: the explicit `--smolfile` override, or the
/// jinn-managed seed (`~/.config/jinn/smolvm/alpine.toml`).
///
/// # Errors
///
/// Returns [`Report<ManifestError>`] when the override names a relative
/// path: the server is a spawned child whose only startup feedback is
/// process death plus stderr, so a malformed flag value aborts the launch
/// loudly rather than silently resolving something else.
pub(crate) fn resolve_manifest_path(
    smolfile: Option<&std::path::Path>,
) -> Result<MarkedPathBuf<Absolute>, error_stack::Report<ManifestError>> {
    use error_stack::{Report, ResultExt};

    let override_path = smolfile.ok_or(Report::new(ManifestError).attach("no override"))?;
    MarkedPathBuf::<Absolute>::new(override_path)
        .change_context(ManifestError)
        .attach(format!(
            "--smolfile must be an absolute path: {}",
            override_path.display()
        ))
}

/// The default manifest path when no `--smolfile` override was given: the
/// reconciler-seeded Alpine profile under the smolvm config dir.
pub(crate) fn default_manifest_path() -> Result<MarkedPathBuf<Absolute>, PathError> {
    default_manifest_path_in(&AppPaths::default())
}

/// The default manifest path under an explicit [`AppPaths`] — the testable
/// half of [`default_manifest_path`].
fn default_manifest_path_in(paths: &AppPaths) -> Result<MarkedPathBuf<Absolute>, PathError> {
    paths.smolvm_dir().join(DEFAULT_MANIFEST_FILE)
}

/// Reads and parses the manifest at `path`.
///
/// The file must exist on disk — the server never boots from in-memory
/// config, so a missing file is an error naming the absolute path (the
/// reconciler seeds it at jinn startup; the message says so).
///
/// # Errors
///
/// Returns [`Report<ManifestError>`] when the file cannot be read, or when
/// it is not valid manifest TOML — including unknown keys, each parse
/// failure naming the offending key or line.
pub(crate) fn load_manifest(
    path: &MarkedPath<Absolute>,
) -> Result<(MachineManifest, String), error_stack::Report<ManifestError>> {
    use error_stack::ResultExt;

    let contents = std::fs::read_to_string(path.as_path())
        .change_context(ManifestError)
        .attach(format!(
            "no machine manifest at {} — it is seeded at jinn startup \
             (restore with `jinn install --builtins` if it was deleted)",
            path.as_path().display()
        ))?;

    let manifest = toml::from_str::<MachineManifest>(&contents)
        .change_context(ManifestError)
        .attach(format!(
            "invalid machine manifest at {}",
            path.as_path().display()
        ))?;

    Ok((manifest, contents))
}

/// Resolves, reads, and parses the manifest in one step: `--smolfile`
/// override when given, else the jinn-managed seed.
///
/// # Errors
///
/// Returns [`Report<ManifestError>`] when the override is relative, the
/// file is missing, or the TOML is invalid.
pub(crate) fn resolve_manifest(
    smolfile: Option<&std::path::Path>,
) -> Result<(MarkedPathBuf<Absolute>, MachineManifest, String), error_stack::Report<ManifestError>>
{
    use error_stack::ResultExt;

    let path = match smolfile {
        Some(_) => resolve_manifest_path(smolfile)?,
        None => default_manifest_path()
            .map_err(|error| error_stack::Report::from(error).change_context(ManifestError))
            .attach("failed to build the default manifest path")?,
    };
    let (manifest, contents) = load_manifest(&path.as_marked_path())?;
    Ok((path, manifest, contents))
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unwrap_used,
        reason = "test code"
    )]

    use super::*;
    use marked_path::MarkedPathAccess;

    /// Writes `contents` as a manifest into a fresh temp dir and returns
    /// the typed path.
    fn manifest_at(contents: &str) -> (jinn_testutil::JinnTempDir, MarkedPathBuf<Absolute>) {
        let dir = jinn_testutil::tempdir();
        let path = dir
            .path
            .join("profile.toml")
            .expect("temp root joins a name");
        std::fs::write(path.as_path(), contents).expect("write manifest");
        (dir, path)
    }

    #[rstest::rstest]
    #[test]
    fn load_parses_a_wellformed_manifest() {
        // Given a well-formed manifest on disk.
        let (_dir, path) = manifest_at(
            "image = \"alpine:latest\"\ncpus = 2\nmemory = 2048\nnet = true\n\
             init = [\"apk add bash\"]\n",
        );

        // When loading it.
        let (manifest, contents) = load_manifest(&path.as_marked_path()).expect("load");

        // Then every field reads back.
        assert_eq!(manifest.image, "alpine:latest");
        assert_eq!(manifest.cpus, 2);
        assert_eq!(manifest.memory, 2048);
        assert!(manifest.net);
        assert_eq!(manifest.init, vec!["apk add bash"]);
        // And the verbatim contents ride along.
        assert!(contents.contains("alpine:latest"));
    }

    #[rstest::rstest]
    #[test]
    fn init_defaults_to_empty_when_absent() {
        // Given a manifest with no init list.
        let (_dir, path) =
            manifest_at("image = \"alpine:latest\"\ncpus = 1\nmemory = 512\nnet = false\n");

        // When loading it.
        let (manifest, _contents) = load_manifest(&path.as_marked_path()).expect("load");

        // Then init is empty, not a missing field.
        assert_eq!(manifest.init, [] as [String; 0]);
    }

    #[rstest::rstest]
    #[test]
    fn load_names_the_path_when_the_file_is_missing() {
        // Given a path that names no file.
        let dir = jinn_testutil::tempdir();
        let path = dir
            .path
            .join("absent.toml")
            .expect("temp root joins a name");

        // When loading it.
        let result = load_manifest(&path.as_marked_path());

        // Then the error names the absolute path.
        let report = result.expect_err("missing manifest must fail");
        let message = format!("{report:?}");
        assert!(
            message.contains(path.as_path().to_str().expect("temp path is utf-8")),
            "error names the missing path: {message}"
        );
        // And the message says the file is seeded at startup.
        assert!(
            message.contains("seeded at jinn startup"),
            "error explains the seed: {message}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn unknown_keys_are_refused_and_named() {
        // Given a manifest carrying a misspelled key.
        let (_dir, path) = manifest_at(
            "image = \"alpine:latest\"\ncpus = 2\nmemory = 2048\nnet = true\n\
             innit = [\"apk add bash\"]\n",
        );

        // When loading it.
        let result = load_manifest(&path.as_marked_path());

        // Then the parse fails naming the unknown key.
        let report = result.expect_err("unknown key must fail");
        let message = format!("{report:?}");
        assert!(
            message.contains("innit"),
            "error names the unknown key: {message}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn resolve_manifest_prefers_the_override() {
        // Given a manifest on disk under a temp root.
        let (_dir, path) =
            manifest_at("image = \"debian:trixie\"\ncpus = 4\nmemory = 4096\nnet = true\n");

        // When resolving with that path as the override.
        let (resolved, manifest, _contents) =
            resolve_manifest(Some(path.as_path())).expect("resolve");

        // Then the override won: the resolved path is the temp file and the
        // parsed profile is its content, not the shipped Alpine seed.
        assert_eq!(resolved, path);
        assert_eq!(manifest.image, "debian:trixie");
    }

    #[rstest::rstest]
    #[test]
    fn a_relative_override_is_rejected_loudly() {
        // Given a relative --smolfile value.
        let relative = std::path::Path::new("profiles/relative.toml");

        // When resolving it.
        let result = resolve_manifest(Some(relative));

        // Then it is rejected, naming the value and the absolute rule —
        // the child's stderr names the mistake.
        let report = result.expect_err("relative override must fail");
        let message = format!("{report:?}");
        assert!(message.contains("absolute"), "names the rule: {message}");
        assert!(
            message.contains("profiles/relative.toml"),
            "names the value: {message}"
        );
    }

    /// The shipped `res/smolvm/alpine.toml` parses with the real parser —
    /// a drifted shipped file would fail every default boot, so it is
    /// pinned here against the compile-time-embedded bytes.
    #[rstest::rstest]
    #[test]
    fn shipped_alpine_manifest_parses_with_the_real_parser() {
        // Given the shipped manifest bytes, embedded at compile time.
        static SHIPPED: &str = include_str!("../../../res/smolvm/alpine.toml");

        // When parsing them with the production parser.
        let manifest: MachineManifest =
            toml::from_str(SHIPPED).expect("shipped alpine.toml parses");

        // Then it names the Alpine profile the server documents.
        assert_eq!(manifest.image, "alpine:latest");
        assert!(manifest.net, "init needs the network to install packages");
        // And the init list installs the toolchain vm_run documents:
        // bash (the vm_run shell), git, and python3.
        let init = manifest.init.join("\n");
        assert!(init.contains("bash"), "init installs bash: {init}");
        assert!(init.contains("git"), "init installs git: {init}");
        assert!(init.contains("python3"), "init installs python3: {init}");
    }

    /// Default resolution targets the seeded file under the smolvm config
    /// dir of the given paths root — the disk path is the real resolution
    /// target, not an embedded copy.
    #[rstest::rstest]
    #[test]
    fn default_resolution_targets_the_seeded_file_under_the_paths_root() {
        // Given an AppPaths rooted at a temp tree.
        let root = jinn_testutil::tempdir();
        let paths = AppPaths::new_in(&root.path.as_marked_path());

        // When resolving the default manifest path.
        let path = default_manifest_path_in(&paths).expect("resolve default");

        // Then it is <root>/config/jinn/smolvm/alpine.toml.
        assert_eq!(
            path,
            root.path
                .join_all(["config", "jinn", "smolvm"])
                .expect("root joins bare names")
                .join(DEFAULT_MANIFEST_FILE)
                .expect("typed parent joins a bare name")
        );
    }
}
