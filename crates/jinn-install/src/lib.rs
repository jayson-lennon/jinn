//! Default config seeding and bundled-resource reconciliation.
//!
//! Every resource under `res/` is embedded at compile time (see
//! [`bundled`]), so the binary is self-contained.
//!
//! Two surfaces use the embed:
//!
//! - [`seed_configs_to`] (`jinn install`) writes the three config files —
//!   `jinn.toml`, `providers.toml`, `stream_rules.toml` — each **exactly
//!   once**, only when it does not exist. An existing file is never read or
//!   modified, so user edits survive and a malformed file never fails the
//!   command. It seeds no `res/` content.
//! - [`reconcile`] (every launch, from composition) heals bundled-resource
//!   drift between the embedded tree, the on-disk copies, and the
//!   state-dir manifest; [`restore_builtins`] (`jinn install --builtins`)
//!   is its explicit overwrite-everything counterpart. See
//!   [`reconcile`] for the three-way rule table.

mod bundled;
mod reconcile;

pub use reconcile::{
    ExpectedFile, ReconcileAction, ReconcileCounts, ReconcileError, ReconcileReport, reconcile,
    restore_builtins,
};

use bundled::{Bundled, bundled_catalogue};
use error_stack::Report;
use error_stack::ResultExt;
use jinn_preferences_config::StreamRulesSeedOutcome;
use marked_path::{Absolute, MarkedPath, MarkedPathAccess, MarkedPathBuf};
use wherror::Error;

/// Error returned by [`seed_configs_to`] and [`expected_catalogue`].
#[derive(Debug, Error)]
#[error(debug)]
pub struct InstallError;

/// Outcome for one of the config files a seed writes.
///
/// Every file is written exactly once — only when it does not exist. An
/// existing file is never read or modified: the user's edits always win.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeedOutcome {
    /// The file did not exist and was created this run (the comment-rich
    /// default template).
    Created(MarkedPathBuf<Absolute>),
    /// The file already existed and was never read or modified this run.
    Untouched(MarkedPathBuf<Absolute>),
}

/// The result of a full config seed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedReport {
    /// Outcome for the user preferences file (`jinn.toml`).
    pub jinn_toml: SeedOutcome,
    /// Outcome for the providers file (`providers.toml`).
    pub providers_toml: SeedOutcome,
    /// Outcome for the shipped stream-rules file, written once next to
    /// `jinn.toml`.
    pub stream_rules: StreamRulesSeedOutcome,
}

/// Seeds the three config files into their standard locations.
///
/// - `jinn.toml` at `prefs_path`, from the preferences template.
/// - `providers.toml` beside it, from the provider template.
/// - `stream_rules.toml` beside `jinn.toml`, from the stream-rules template.
///
/// Each is created only when missing; an existing file is never read or
/// modified, so a malformed file never fails the seed. No bundled `res/`
/// resource is written — resource placement is the reconciler's job.
///
/// # Errors
///
/// Returns [`Report<InstallError>`] if a template write fails on a
/// fresh-create path.
///
/// # Panics
///
/// Panics if the preferences path has no parent, which cannot happen for an
/// absolute path: an absolute path always names a parent directory (the
/// filesystem root is its own).
#[expect(
    clippy::expect_used,
    reason = "parent of an absolute path is absolute by construction"
)]
pub fn seed_configs_to(
    prefs_path: &MarkedPathBuf<Absolute>,
    providers_path: &MarkedPathBuf<Absolute>,
) -> Result<SeedReport, Report<InstallError>> {
    let jinn_toml = seed_once(prefs_path, || {
        // Fresh-create path: write the comment-rich default template as
        // bytes. Deliberately NOT a round-trip through a config struct —
        // the template is documentation, and serializing a struct would
        // strip every comment it ships with.
        jinn_preferences_config::create_default_preferences_to(prefs_path)
            .change_context(InstallError)
            .attach("failed to create jinn.toml with the default template")
    })?;

    let providers_toml = seed_once(providers_path, || {
        jinn_provider_config::init_default_providers_to(providers_path, true)
            .map(|_| ())
            .change_context(InstallError)
            .attach("failed to create providers.toml with the default template")
    })?;

    // The stream-rules file lives beside `jinn.toml`, which is what the
    // runtime read path resolves against.
    // `parent()` is `None` only for the filesystem root, which is then its
    // own parent for the sibling lookup.
    let stream_rules_path = {
        let prefs_parent = prefs_path.parent().map_or_else(
            || prefs_path.clone(),
            |p| {
                MarkedPathBuf::<Absolute>::new(p.as_path())
                    .expect("parent of an absolute path is absolute")
            },
        );
        prefs_parent
            .join(jinn_preferences_config::STREAM_RULES_FILE_NAME)
            .change_context(InstallError)
            .attach("failed to build stream_rules.toml path")?
    };
    let stream_rules = jinn_preferences_config::init_stream_rules_for_install(&stream_rules_path)
        .change_context(InstallError)
        .attach("failed to create stream_rules.toml with the embedded template")?;

    Ok(SeedReport {
        jinn_toml,
        providers_toml,
        stream_rules,
    })
}

/// Writes a config file only when it is missing, mapping the write to a
/// [`SeedOutcome`].
///
/// The `write` closure runs on the fresh-create path only; `force: true`
/// inside it is safe because the existence gate has already excluded the
/// overwrite case.
fn seed_once(
    path: &MarkedPathBuf<Absolute>,
    write: impl FnOnce() -> Result<(), Report<InstallError>>,
) -> Result<SeedOutcome, Report<InstallError>> {
    if path.exists() {
        return Ok(SeedOutcome::Untouched(path.to_owned()));
    }
    write()?;
    Ok(SeedOutcome::Created(path.to_owned()))
}

/// Derives the reconciler's expected set from the embedded `res/` tree:
/// one [`ExpectedFile`] per bundled file, keyed by its `res/`-relative
/// path with the compile-time-embedded bytes.
///
/// Editing, adding, or removing files under the five `res/` families ships
/// through this derivation with zero code changes: the catalogue is walked
/// fresh on every call.
///
/// # Errors
///
/// Returns [`Report<InstallError>`] if any embedded file sits under a
/// top-level directory that names no resource kind.
pub fn expected_catalogue() -> Result<Vec<ExpectedFile<'static>>, Report<InstallError>> {
    let catalogue = bundled_catalogue()?;
    let expected = catalogue
        .iter()
        .map(|resource: &Bundled| {
            Ok(ExpectedFile {
                path: resource
                    .res_relative()
                    .change_context(InstallError)
                    .attach("bundled resource path is not a valid relative path")?,
                bytes: resource.contents(),
            })
        })
        .collect::<Result<Vec<_>, Report<InstallError>>>()?;
    Ok(expected)
}

/// Builds a [`Destinations`] from the standard [`AppPaths`]-derived roots:
/// themes/personas/prompts/workflows under the config dir, skills under the
/// agent dir.
///
/// The composition layer and both commands use this so there is exactly one
/// spelling of "where bundled resources live on this machine".
#[must_use]
pub fn destinations_from_paths(app_paths: &jinn_common::app_paths::AppPaths) -> Destinations {
    Destinations::new(
        app_paths.themes_dir().as_marked_path(),
        app_paths.personas_dir().as_marked_path(),
        app_paths.prompts_dir().as_marked_path(),
        app_paths.workflows_dir().as_marked_path(),
        app_paths.skills_dir().as_marked_path(),
    )
}

/// Relative destinations for the five resource kinds.
///
/// Each field is a root directory: themes/personas/prompts/workflows live
/// under the config dir (`~/.config/jinn`), skills live under the agent dir
/// (`~/.agents/skills`). Passed by value into the reconciler so tests can
/// point at temp dirs.
#[derive(Debug, Clone)]
pub struct Destinations {
    themes: MarkedPathBuf<Absolute>,
    personas: MarkedPathBuf<Absolute>,
    prompts: MarkedPathBuf<Absolute>,
    workflows: MarkedPathBuf<Absolute>,
    skills: MarkedPathBuf<Absolute>,
}

impl Destinations {
    /// Creates a destination set from the five root directories.
    #[must_use]
    pub fn new(
        themes: MarkedPath<Absolute>,
        personas: MarkedPath<Absolute>,
        prompts: MarkedPath<Absolute>,
        workflows: MarkedPath<Absolute>,
        skills: MarkedPath<Absolute>,
    ) -> Self {
        Self {
            themes: themes.to_owned(),
            personas: personas.to_owned(),
            prompts: prompts.to_owned(),
            workflows: workflows.to_owned(),
            skills: skills.to_owned(),
        }
    }

    /// The root directory themes install into.
    pub(crate) fn themes(&self) -> MarkedPath<'_, Absolute> {
        self.themes.as_marked_path()
    }

    /// The root directory personas install into.
    pub(crate) fn personas(&self) -> MarkedPath<'_, Absolute> {
        self.personas.as_marked_path()
    }

    /// The root directory prompts install into.
    pub(crate) fn prompts(&self) -> MarkedPath<'_, Absolute> {
        self.prompts.as_marked_path()
    }

    /// The root directory workflows install into — one subdirectory per
    /// workflow, the shape `scan_workflows` discovers.
    pub(crate) fn workflows(&self) -> MarkedPath<'_, Absolute> {
        self.workflows.as_marked_path()
    }

    /// The root directory skills install into.
    pub(crate) fn skills(&self) -> MarkedPath<'_, Absolute> {
        self.skills.as_marked_path()
    }
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

    /// An environment for the config-seed tests: a temp dir holding
    /// `jinn.toml`, `providers.toml`, and `stream_rules.toml`.
    struct SeedEnv {
        prefs_path: MarkedPathBuf<Absolute>,
        providers_path: MarkedPathBuf<Absolute>,
        _dir: jinn_testutil::JinnTempDir,
    }

    impl SeedEnv {
        fn fresh() -> Self {
            let dir = jinn_testutil::tempdir();
            let prefs_path = dir.path.join("jinn.toml").expect("temp root joins a name");
            let providers_path = dir
                .path
                .join("providers.toml")
                .expect("temp root joins a name");
            Self {
                prefs_path,
                providers_path,
                _dir: dir,
            }
        }

        fn seed(&self) -> SeedReport {
            seed_configs_to(&self.prefs_path, &self.providers_path).expect("seed configs")
        }

        /// The stream-rules path: beside jinn.toml.
        fn rules_path(&self) -> MarkedPathBuf<Absolute> {
            self.prefs_path
                .parent()
                .unwrap_or(self.prefs_path.as_marked_path())
                .join(jinn_preferences_config::STREAM_RULES_FILE_NAME)
                .expect("typed parent joins a bare name")
        }
    }

    #[rstest::rstest]
    #[test]
    fn seed_creates_all_three_config_files_when_missing() {
        // Given a fresh environment with no config files.
        let env = SeedEnv::fresh();

        // When seeding.
        let report = env.seed();

        // Then all three were created.
        assert_eq!(
            report.jinn_toml,
            SeedOutcome::Created(env.prefs_path.clone())
        );
        assert_eq!(
            report.providers_toml,
            SeedOutcome::Created(env.providers_path.clone())
        );
        assert_eq!(
            report.stream_rules,
            StreamRulesSeedOutcome::Created(env.rules_path())
        );
        // And each exists on disk.
        assert!(env.prefs_path.exists());
        assert!(env.providers_path.exists());
        assert!(env.rules_path().exists());
    }

    #[rstest::rstest]
    #[test]
    fn seed_never_touches_existing_config_files() {
        // Given config files already present with user content (malformed
        // on purpose: they must never even be read).
        let env = SeedEnv::fresh();
        std::fs::write(&env.prefs_path, "# my jinn\nNOT [valid").unwrap();
        std::fs::write(&env.providers_path, "# my providers\nNOT [valid").unwrap();
        let original = "# my rules\nNOT [valid toml\n";
        std::fs::write(env.rules_path(), original).unwrap();

        // When seeding.
        let report = env.seed();

        // Then all three are reported untouched.
        assert_eq!(
            report.jinn_toml,
            SeedOutcome::Untouched(env.prefs_path.clone())
        );
        assert_eq!(
            report.providers_toml,
            SeedOutcome::Untouched(env.providers_path.clone())
        );
        assert_eq!(
            report.stream_rules,
            StreamRulesSeedOutcome::Untouched(env.rules_path())
        );
        // And every file is byte-identical — the proof nothing was read or
        // rewritten.
        assert_eq!(
            std::fs::read_to_string(&env.prefs_path).unwrap(),
            "# my jinn\nNOT [valid"
        );
        assert_eq!(
            std::fs::read_to_string(&env.providers_path).unwrap(),
            "# my providers\nNOT [valid"
        );
        assert_eq!(std::fs::read_to_string(env.rules_path()).unwrap(), original);
    }

    #[rstest::rstest]
    #[test]
    fn seed_is_idempotent() {
        // Given a completed first seed.
        let env = SeedEnv::fresh();
        env.seed();

        // When seeding again.
        let second = env.seed();

        // Then nothing is reported created.
        assert!(matches!(second.jinn_toml, SeedOutcome::Untouched(_)));
        assert!(matches!(second.providers_toml, SeedOutcome::Untouched(_)));
        assert!(matches!(
            second.stream_rules,
            StreamRulesSeedOutcome::Untouched(_)
        ));
    }

    #[rstest::rstest]
    #[test]
    fn seed_creates_missing_sibling_when_the_other_files_exist() {
        // Given providers.toml and stream_rules.toml present but no jinn.toml.
        let env = SeedEnv::fresh();
        std::fs::write(&env.providers_path, "# providers\n").unwrap();
        std::fs::write(env.rules_path(), "# rules\n").unwrap();

        // When seeding.
        let report = env.seed();

        // Then only jinn.toml was created; the others stay untouched.
        assert_eq!(
            report.jinn_toml,
            SeedOutcome::Created(env.prefs_path.clone())
        );
        assert_eq!(
            report.providers_toml,
            SeedOutcome::Untouched(env.providers_path.clone())
        );
        assert_eq!(
            report.stream_rules,
            StreamRulesSeedOutcome::Untouched(env.rules_path())
        );
        assert_eq!(
            std::fs::read_to_string(env.rules_path()).unwrap(),
            "# rules\n"
        );
    }

    /// The expected catalogue is derived from the embedded tree: every path
    /// classifies to a resource kind — the same component-wise classifier
    /// the write path uses — and the set is non-empty.
    #[rstest::rstest]
    #[test]
    fn expected_catalogue_covers_every_embedded_file() {
        // Given the embedded resources.
        // When deriving the expected set.
        let expected = expected_catalogue().expect("derive expected set");

        // Then it is non-empty and every path names a known kind.
        assert!(!expected.is_empty(), "the embedded tree is not empty");
        for file in &expected {
            let (_kind, _rest) =
                bundled::res_relative_parts(file.path.as_path()).unwrap_or_else(|| {
                    panic!(
                        "expected path names no resource kind: {}",
                        file.path.as_path().display()
                    )
                });
            assert!(
                !file.bytes.is_empty(),
                "{} ships empty",
                file.path.as_path().display()
            );
        }
    }

    /// The expected catalogue's paths resolve under the `AppPaths`-derived
    /// destination roots exactly as the reconciliation write path resolves
    /// them — kind classifier, then typed join — so the two derivations
    /// provably agree.
    #[rstest::rstest]
    #[test]
    fn expected_catalogue_paths_resolve_under_kind_roots() {
        // Given an AppPaths rooted in a temp dir and its derived destinations.
        let root = jinn_testutil::tempdir();
        let destinations = destinations_from_paths(&jinn_common::app_paths::AppPaths::new_in(
            &root.path.as_marked_path(),
        ));
        let expected = expected_catalogue().expect("derive expected set");

        // When resolving every expected path through the kind classifier and
        // the kind's destination root.
        // Then each resolution succeeds and lands beneath that kind's root —
        // the `res/` kind directories and the destination roots agree.
        for file in &expected {
            let (kind, rest) =
                bundled::res_relative_parts(file.path.as_path()).unwrap_or_else(|| {
                    panic!(
                        "expected path names no resource kind: {}",
                        file.path.as_path().display()
                    )
                });
            let resolved = kind.root(&destinations).join(rest).unwrap_or_else(|error| {
                panic!(
                    "expected path {} does not resolve under its kind root: {error}",
                    file.path.as_path().display()
                )
            });
            assert!(
                resolved
                    .as_path()
                    .starts_with(kind.root(&destinations).as_path()),
                "{} resolves outside the {} root",
                resolved.as_path().display(),
                kind.dir_name()
            );
        }
    }
}
