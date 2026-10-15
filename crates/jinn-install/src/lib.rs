//! Default resource installation — seeds themes, personas, prompts, and
//! skills into the user's config and agent directories.
//!
//! Every resource under `res/` is embedded at compile time (see
//! [`bundled`]), so the binary is self-contained.
//!
//! [`install_defaults_to`] (`jinn install`) is a pure seeder. Payload files
//! follow skip/`--force` rules; `jinn.toml` and the stream-rules file are
//! written **exactly once**, only when they do not exist. An existing file
//! is never read or modified — even with `--force` — so user edits survive
//! and a malformed file never fails the install.

mod bundled;

use std::path::{Path, PathBuf};

use error_stack::{Report, ResultExt};
use jinn_preferences_config::StreamRulesSeedOutcome;
use wherror::Error;

use bundled::{Bundled, bundled_catalogue};

/// Relative destinations for the four resource kinds.
///
/// Each field is a root directory: themes/personas/prompts live under the
/// config dir (`~/.config/jinn`), skills live under the agent dir
/// (`~/.agents/skills`). Passed by value into [`install_defaults_to`] so
/// tests can point at temp dirs.
#[derive(Debug, Clone)]
pub struct Destinations {
    themes: PathBuf,
    personas: PathBuf,
    prompts: PathBuf,
    skills: PathBuf,
}

impl Destinations {
    /// Creates a destination set from the four root directories.
    #[must_use]
    pub fn new(themes: PathBuf, personas: PathBuf, prompts: PathBuf, skills: PathBuf) -> Self {
        Self {
            themes,
            personas,
            prompts,
            skills,
        }
    }

    /// The root directory themes install into.
    pub(crate) fn themes(&self) -> &Path {
        &self.themes
    }

    /// The root directory personas install into.
    pub(crate) fn personas(&self) -> &Path {
        &self.personas
    }

    /// The root directory prompts install into.
    pub(crate) fn prompts(&self) -> &Path {
        &self.prompts
    }

    /// The root directory skills install into.
    pub(crate) fn skills(&self) -> &Path {
        &self.skills
    }
}

/// Outcome of installing one resource.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallOutcome {
    /// Resource was written to a previously-missing path.
    Created(PathBuf),
    /// Resource already existed and was left untouched.
    Skipped(PathBuf),
    /// Resource already existed and was overwritten in place.
    Overwritten(PathBuf),
}

impl InstallOutcome {
    /// The full destination path this outcome refers to.
    #[must_use]
    pub fn path(&self) -> &Path {
        match self {
            InstallOutcome::Created(p)
            | InstallOutcome::Skipped(p)
            | InstallOutcome::Overwritten(p) => p,
        }
    }
}

/// Error returned by [`install_defaults_to`].
#[derive(Debug, Error)]
#[error(debug)]
pub struct InstallError;

/// The result of a full default install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallReport {
    /// Per-resource outcomes in catalogue order (deterministic).
    pub outcomes: Vec<InstallOutcome>,
    /// Outcome for the user preferences file itself.
    pub jinn_toml: JinnTomlOutcome,
    /// Outcome for the shipped stream-rules file, written once next to
    /// `jinn.toml`.
    pub stream_rules: StreamRulesSeedOutcome,
}

/// Outcome for `jinn.toml` during a default install.
///
/// `jinn install` writes the file exactly once — only when it does not
/// exist. An existing file is never read or modified, even under `--force`:
/// the user's `enabled`, `config`, and hand-edited grants always win.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JinnTomlOutcome {
    /// The file did not exist and was created this run (the comment-rich
    /// default template).
    Created(PathBuf),
    /// The file already existed and was never read or modified this run.
    Untouched(PathBuf),
}

/// Installs every bundled default resource into the given destinations.
///
/// Per resource:
/// - If the destination already exists and `overwrite` is false → [`InstallOutcome::Skipped`].
/// - If the destination already exists and `overwrite` is true → the file is replaced, yielding [`InstallOutcome::Overwritten`].
/// - Otherwise → parents are created via `create_dir_all` and the file is
///   written, yielding [`InstallOutcome::Created`].
///
/// Parents are created unconditionally before each write so a missing config
/// tree never surfaces as a "directory does not exist" write error.
///
/// **`jinn.toml` is written exactly once — only when it does not exist.**
/// If the file already exists it is never read or modified — even with
/// `overwrite` — so a malformed `jinn.toml` never fails the install; the
/// caller surfaces this via [`JinnTomlOutcome::Untouched`].
///
/// Outcomes are returned in catalogue order (deterministic), alongside the
/// `jinn.toml` outcome.
///
/// # Errors
///
/// Returns [`Report<InstallError>`] if directory creation or file writing
/// fails, or if preferences cannot be created on the fresh-create path.
pub fn install_defaults_to(
    destinations: &Destinations,
    overwrite: bool,
    prefs_path: &Path,
) -> Result<InstallReport, Report<InstallError>> {
    let prefs_existed = prefs_path.exists();

    let catalogue = bundled_catalogue()?;

    let mut outcomes = Vec::with_capacity(catalogue.len());
    for resource in &catalogue {
        outcomes.push(install_resource(resource, destinations, overwrite)?);
    }

    let jinn_toml = if prefs_existed {
        JinnTomlOutcome::Untouched(prefs_path.to_path_buf())
    } else {
        // Fresh-create path: write the comment-rich default template as
        // bytes. Deliberately NOT a round-trip through a config struct —
        // the template is documentation, and serializing a struct would
        // strip every comment it ships with.
        jinn_preferences_config::create_default_preferences_to(prefs_path)
            .change_context(InstallError)
            .attach("failed to create jinn.toml with the default template")?;
        JinnTomlOutcome::Created(prefs_path.to_path_buf())
    };

    // The stream-rules file follows the same exactly-once contract as
    // `jinn.toml`: written when missing, never read or modified when
    // present — including under `overwrite`. The sibling location is the
    // same directory the preferences file lives in, which is what the
    // runtime read path resolves against.
    let stream_rules = jinn_preferences_config::init_stream_rules_for_install(
        prefs_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(jinn_preferences_config::STREAM_RULES_FILE_NAME),
    )
    .change_context(InstallError)
    .attach("failed to create stream_rules.toml with the embedded template")?;

    Ok(InstallReport {
        outcomes,
        jinn_toml,
        stream_rules,
    })
}

/// Installs a single bundled resource, returning its outcome.
///
/// The payload is written verbatim from its embedded bytes; whether it is valid
/// UTF-8 is irrelevant to installation.
fn install_resource(
    resource: &Bundled,
    destinations: &Destinations,
    overwrite: bool,
) -> Result<InstallOutcome, Report<InstallError>> {
    let destination = resource.destination(destinations);
    let existed = destination.exists();

    if existed && !overwrite {
        return Ok(InstallOutcome::Skipped(destination));
    }

    write_resource(&destination, resource.contents())?;

    Ok(final_outcome(destination, existed))
}

/// Writes `bytes` to `destination`, creating parent directories first.
fn write_resource(destination: &Path, bytes: &[u8]) -> Result<(), Report<InstallError>> {
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)
            .change_context(InstallError)
            .attach("failed to create destination directory")
            .attach(format!("path: {}", parent.display()))?;
    }

    std::fs::write(destination, bytes)
        .change_context(InstallError)
        .attach("failed to write resource")
        .attach(format!("path: {}", destination.display()))
}

/// The outcome for a write that happened: overwritten vs created.
fn final_outcome(destination: PathBuf, existed: bool) -> InstallOutcome {
    if existed {
        InstallOutcome::Overwritten(destination)
    } else {
        InstallOutcome::Created(destination)
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
    use tempfile::TempDir;

    /// Builds a [`Destinations`] rooted at four distinct temp dirs. The
    /// returned temps must outlive the destinations.
    fn fresh_destinations() -> (Destinations, Vec<TempDir>) {
        let themes = TempDir::new().unwrap();
        let personas = TempDir::new().unwrap();
        let prompts = TempDir::new().unwrap();
        let skills = TempDir::new().unwrap();
        let destinations = Destinations::new(
            themes.path().to_path_buf(),
            personas.path().to_path_buf(),
            prompts.path().to_path_buf(),
            skills.path().to_path_buf(),
        );
        let temps = vec![themes, personas, prompts, skills];
        (destinations, temps)
    }

    /// An install environment: fresh destinations plus a prefs file path that
    /// does **not** exist yet (inside its own temp dir), backed by the real
    /// filesystem storage so the auto-create/patch semantics the existence
    /// gate depends on are exercised end to end.
    struct TestEnv {
        destinations: Destinations,
        prefs_path: std::path::PathBuf,
        _temps: Vec<TempDir>,
    }

    impl TestEnv {
        fn fresh() -> Self {
            let (destinations, mut temps) = fresh_destinations();
            let prefs_dir = TempDir::new().unwrap();
            let prefs_path = prefs_dir.path().join("jinn.toml");
            temps.push(prefs_dir);
            Self {
                destinations,
                prefs_path,
                _temps: temps,
            }
        }

        fn run(&self, overwrite: bool) -> InstallReport {
            install_defaults_to(&self.destinations, overwrite, &self.prefs_path).expect("install")
        }
    }

    /// Locates the outcome for a specific resource relative path.
    fn outcome_for<'a>(outcomes: &'a [InstallOutcome], relative: &str) -> &'a InstallOutcome {
        outcomes
            .iter()
            .find(|o| o.path().ends_with(relative))
            .unwrap_or_else(|| panic!("no outcome ending in {relative}"))
    }

    #[rstest::rstest]
    #[test]
    fn install_creates_theme_when_absent() {
        // Given destinations with no existing themes.
        let env = TestEnv::fresh();

        // When installing defaults.
        let report = env.run(false);

        // Then the `default.toml` theme was created.
        let outcome = outcome_for(&report.outcomes, "default.toml");
        assert!(
            matches!(outcome, InstallOutcome::Created(_)),
            "default.toml should be Created"
        );
        // And the file exists with non-empty contents.
        let written = std::fs::read_to_string(outcome.path()).expect("read");
        assert_ne!(written, "");
    }

    #[rstest::rstest]
    #[test]
    fn install_skips_theme_when_present() {
        // Given a destinations dir where `default.toml` already exists.
        let env = TestEnv::fresh();
        let existing = env.destinations.themes().join("default.toml");
        std::fs::create_dir_all(env.destinations.themes()).unwrap();
        std::fs::write(&existing, "PRE-EXISTING").unwrap();

        // When installing defaults.
        let report = env.run(false);

        // Then `default.toml` is skipped (not overwritten).
        let outcome = outcome_for(&report.outcomes, "default.toml");
        assert!(
            matches!(outcome, InstallOutcome::Skipped(_)),
            "default.toml should be Skipped"
        );
        // And the original contents are untouched.
        let contents = std::fs::read_to_string(&existing).expect("read");
        assert_eq!(contents, "PRE-EXISTING");
    }

    #[rstest::rstest]
    #[test]
    fn install_creates_parent_dirs_when_missing() {
        // Given destinations whose root dirs do not exist at all.
        let themes = TempDir::new().unwrap();
        let personas = TempDir::new().unwrap();
        let prompts = TempDir::new().unwrap();
        let skills = TempDir::new().unwrap();
        let prefs_dir = TempDir::new().unwrap();
        // Non-existent subdirs under each temp root.
        let destinations = Destinations::new(
            themes.path().join("themes"),
            personas.path().join("personas"),
            prompts.path().join("prompts"),
            skills.path().join("skills"),
        );
        let prefs_path = prefs_dir.path().join("nested").join("jinn.toml");

        // When installing defaults.
        let result = install_defaults_to(&destinations, false, &prefs_path);

        // Then it succeeds (parents created) rather than erroring.
        assert!(result.is_ok(), "install should create missing parents");
    }

    #[rstest::rstest]
    #[test]
    fn install_creates_persona() {
        // Given destinations with no existing personas.
        let env = TestEnv::fresh();

        // When installing defaults.
        let report = env.run(false);

        // Then `general.md` lands under the personas root.
        let outcome = outcome_for(&report.outcomes, "general.md");
        assert!(
            outcome.path().starts_with(env.destinations.personas()),
            "persona should be under the personas root"
        );
        assert!(
            matches!(outcome, InstallOutcome::Created(_)),
            "persona should be Created"
        );
    }

    #[rstest::rstest]
    #[test]
    fn install_creates_prompt() {
        // Given destinations with no existing prompts.
        let env = TestEnv::fresh();

        // When installing defaults.
        let report = env.run(false);

        // Then `plan.md` lands under the prompts root.
        let outcome = outcome_for(&report.outcomes, "plan.md");
        assert!(
            outcome.path().starts_with(env.destinations.prompts()),
            "prompt should be under the prompts root"
        );
    }

    #[rstest::rstest]
    #[test]
    fn install_creates_research_prompt() {
        // Given destinations with no existing prompts.
        let env = TestEnv::fresh();

        // When installing defaults.
        let report = env.run(false);

        // Then `research.md` lands under the prompts root.
        let outcome = outcome_for(&report.outcomes, "research.md");
        assert!(
            outcome.path().starts_with(env.destinations.prompts()),
            "prompt should be under the prompts root"
        );
        // And it reports Created.
        assert!(
            matches!(outcome, InstallOutcome::Created(_)),
            "research.md should be Created"
        );
    }

    #[rstest::rstest]
    #[test]
    fn install_preserves_skill_subdir() {
        // Given destinations with no existing skills.
        let env = TestEnv::fresh();

        // When installing defaults.
        let report = env.run(false);

        // Then the nested skill keeps its `<name>/SKILL.md` structure.
        let outcome = outcome_for(&report.outcomes, "phased-task-loop/SKILL.md");
        assert!(
            matches!(outcome, InstallOutcome::Created(_)),
            "skill should be Created"
        );
        // And the file exists at the nested path.
        assert!(outcome.path().is_file());
    }

    #[rstest::rstest]
    #[test]
    fn install_creates_micro_task_loop_skill() {
        // Given destinations with no existing skills.
        let env = TestEnv::fresh();

        // When installing defaults.
        let report = env.run(false);

        // Then the micro-task-loop skill was created under the skills root.
        let outcome = outcome_for(&report.outcomes, "micro-task-loop/SKILL.md");
        assert!(
            outcome.path().starts_with(env.destinations.skills()),
            "skill should be under the skills root"
        );
        assert!(
            matches!(outcome, InstallOutcome::Created(_)),
            "skill should be Created"
        );
        // And the file exists at the nested path with non-empty contents.
        assert!(outcome.path().is_file());
        assert_ne!(std::fs::read_to_string(outcome.path()).expect("read"), "");
    }

    #[rstest::rstest]
    #[test]
    fn install_is_idempotent() {
        // Given a fresh set of destinations.
        let env = TestEnv::fresh();

        // When running install a second time (after a first full run).
        env.run(false);
        let second = env.run(false);

        // Then every outcome is Skipped and nothing reports Created.
        assert!(
            second
                .outcomes
                .iter()
                .all(|o| matches!(o, InstallOutcome::Skipped(_))),
            "second run must skip everything"
        );
        // And jinn.toml is reported untouched on the second run.
        assert!(
            matches!(second.jinn_toml, JinnTomlOutcome::Untouched(_)),
            "second run must not rewrite jinn.toml"
        );
    }

    #[rstest::rstest]
    #[test]
    fn install_outcomes_include_full_paths() {
        // Given a fresh set of destinations.
        let env = TestEnv::fresh();

        // When installing defaults.
        let report = env.run(false);

        // Then every outcome path is absolute (full path for the CLI to print).
        assert!(
            report.outcomes.iter().all(|o| o.path().is_absolute()),
            "every outcome must carry an absolute path"
        );
        // And the install covered at least one bundled resource.
        assert_ne!(report.outcomes, [] as [InstallOutcome; 0]);
    }

    #[rstest::rstest]
    #[test]
    fn install_overwrites_theme_when_force() {
        // Given a destinations dir where `default.toml` already exists.
        let env = TestEnv::fresh();
        let existing = env.destinations.themes().join("default.toml");
        std::fs::create_dir_all(env.destinations.themes()).unwrap();
        std::fs::write(&existing, "PRE-EXISTING").unwrap();

        // When installing defaults with overwrite enabled.
        let report = env.run(true);

        // Then `default.toml` is reported as overwritten (not skipped).
        let outcome = outcome_for(&report.outcomes, "default.toml");
        assert!(
            matches!(outcome, InstallOutcome::Overwritten(_)),
            "default.toml should be Overwritten"
        );
    }

    #[rstest::rstest]
    #[test]
    fn install_force_replaces_with_bundled_contents() {
        // Given a destinations dir where `default.toml` holds stale contents,
        // and a second destinations dir installed fresh to capture the bundled bytes.
        let env = TestEnv::fresh();
        let bundled_env = TestEnv::fresh();
        let existing = env.destinations.themes().join("default.toml");
        std::fs::create_dir_all(env.destinations.themes()).unwrap();
        std::fs::write(&existing, "PRE-EXISTING").unwrap();
        bundled_env.run(false);
        let bundled_default = bundled_env.destinations.themes().join("default.toml");
        let expected = std::fs::read_to_string(&bundled_default).expect("read bundled");

        // When installing with overwrite enabled.
        env.run(true);

        // Then the overwritten file matches the bundled contents, not the stale value.
        let contents = std::fs::read_to_string(&existing).expect("read");
        assert_eq!(contents, expected);
    }

    #[rstest::rstest]
    #[test]
    fn install_idempotent_under_force() {
        // Given a fully-installed destinations dir (files already match the bundled bytes).
        let env = TestEnv::fresh();
        env.run(false);

        // When installing again with overwrite enabled.
        let second = env.run(true);

        // Then every outcome is Overwritten — overwrite rewrites unconditionally,
        // with no content-diff short-circuit that would report Skipped.
        assert!(
            second
                .outcomes
                .iter()
                .all(|o| matches!(o, InstallOutcome::Overwritten(_))),
            "force run must overwrite everything, even unchanged files"
        );
        // And jinn.toml remains untouched even under force.
        assert!(
            matches!(second.jinn_toml, JinnTomlOutcome::Untouched(_)),
            "force must never rewrite an existing jinn.toml"
        );
    }

    #[rstest::rstest]
    #[test]
    fn install_creates_jinn_toml_once_on_fresh_env() {
        // Given a fresh environment with no jinn.toml.
        let env = TestEnv::fresh();
        assert!(!env.prefs_path.exists());

        // When installing defaults.
        let report = env.run(false);

        // Then jinn.toml is reported Created.
        assert_eq!(
            report.jinn_toml,
            JinnTomlOutcome::Created(env.prefs_path.clone())
        );
        // And the file exists on disk.
        assert!(env.prefs_path.exists(), "jinn.toml must be created on disk");
    }

    #[rstest::rstest]
    #[test]
    fn install_reports_untouched_jinn_toml_when_file_exists() {
        // Given an environment where jinn.toml already exists (even malformed).
        let env = TestEnv::fresh();
        if let Some(parent) = env.prefs_path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&env.prefs_path, "NOT [valid toml").unwrap();

        // When installing defaults.
        let report = env.run(false);

        // Then jinn.toml is reported Untouched.
        assert_eq!(
            report.jinn_toml,
            JinnTomlOutcome::Untouched(env.prefs_path.clone())
        );
        // And the malformed file is left byte-identical — which is itself the
        // proof that nothing was registered (a reload-based assertion is
        // impossible: the file does not parse).
        let on_disk = std::fs::read_to_string(&env.prefs_path).expect("read");
        assert_eq!(on_disk, "NOT [valid toml");
    }

    #[rstest::rstest]
    #[test]
    fn install_force_leaves_existing_jinn_toml_byte_identical() {
        // Given an environment where jinn.toml exists with a user customization
        // and a theme file already exists on disk.
        let env = TestEnv::fresh();
        if let Some(parent) = env.prefs_path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let original = "# my edits\n[compaction]\nthreshold = 0.4\n";
        std::fs::write(&env.prefs_path, original).unwrap();
        let existing = env.destinations.themes().join("default.toml");
        std::fs::create_dir_all(env.destinations.themes()).unwrap();
        std::fs::write(&existing, "PRE-EXISTING").unwrap();

        // When installing defaults with force.
        let report = env.run(true);

        // Then jinn.toml is Untouched and byte-identical on disk.
        assert_eq!(
            report.jinn_toml,
            JinnTomlOutcome::Untouched(env.prefs_path.clone())
        );
        let on_disk = std::fs::read_to_string(&env.prefs_path).expect("read");
        assert_eq!(on_disk, original);
        // And resource files were still overwritten.
        let theme_outcome = outcome_for(&report.outcomes, "default.toml");
        assert!(
            matches!(theme_outcome, InstallOutcome::Overwritten(_)),
            "resource files must still follow --force"
        );
    }

    #[rstest::rstest]
    #[test]
    fn install_creates_the_stream_rules_file_on_a_fresh_env() {
        // Given a fresh environment with no stream_rules.toml.
        let env = TestEnv::fresh();

        // When installing defaults.
        let report = env.run(false);

        // Then the rules file is created beside jinn.toml with the embedded
        // template.
        let expected = env
            .prefs_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(jinn_preferences_config::STREAM_RULES_FILE_NAME);
        assert_eq!(
            report.stream_rules,
            StreamRulesSeedOutcome::Created(expected.clone())
        );
        let on_disk = std::fs::read_to_string(&expected).expect("read rules file");
        assert_eq!(
            on_disk,
            jinn_preferences_config::STREAM_RULES_TEMPLATE,
            "the created file is the embedded template, verbatim"
        );
    }

    #[rstest::rstest]
    #[test]
    fn install_never_touches_an_existing_stream_rules_file() {
        // Given an environment where stream_rules.toml exists with user
        // content (malformed on purpose: it must never even be read).
        let env = TestEnv::fresh();
        let rules_path = env
            .prefs_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(jinn_preferences_config::STREAM_RULES_FILE_NAME);
        std::fs::create_dir_all(rules_path.parent().unwrap()).unwrap();
        let original = "# my rules\nNOT [valid toml\n";
        std::fs::write(&rules_path, original).unwrap();

        // When installing defaults.
        let report = env.run(false);

        // Then it is reported untouched.
        assert_eq!(
            report.stream_rules,
            StreamRulesSeedOutcome::Untouched(rules_path.clone())
        );
        // And the file is byte-identical — the proof nothing read or
        // rewrote it.
        assert_eq!(
            std::fs::read_to_string(&rules_path).expect("read"),
            original
        );
    }

    #[rstest::rstest]
    #[test]
    fn install_force_leaves_an_existing_stream_rules_file_byte_identical() {
        // Given an installed environment whose rules file carries user edits.
        let env = TestEnv::fresh();
        env.run(false);
        let rules_path = env
            .prefs_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(jinn_preferences_config::STREAM_RULES_FILE_NAME);
        let edited = "# my edits\n[[stream_rules.entry]]\nname = 'mine'\nconditions = ['X']\nscopes = ['tool:bash']\nbody = 'x'\n";
        std::fs::write(&rules_path, edited).unwrap();
        let existing = env.destinations.themes().join("default.toml");
        std::fs::create_dir_all(env.destinations.themes()).unwrap();
        std::fs::write(&existing, "PRE-EXISTING").unwrap();

        // When installing with force.
        let report = env.run(true);

        // Then resource files were overwritten but the rules file is
        // untouched and byte-identical: force does not apply to it, the
        // same contract as jinn.toml.
        let theme_outcome = outcome_for(&report.outcomes, "default.toml");
        assert!(matches!(theme_outcome, InstallOutcome::Overwritten(_)));
        assert_eq!(
            report.stream_rules,
            StreamRulesSeedOutcome::Untouched(rules_path.clone())
        );
        assert_eq!(std::fs::read_to_string(&rules_path).expect("read"), edited);
    }

    /// A nested skill reference (a file beneath a skill's own directory)
    /// installs with its structure intact, at the same relative path under the
    /// skills root.
    #[rstest::rstest]
    #[test]
    fn install_preserves_nested_skill_reference_path() {
        // Given a fresh set of destinations.
        let env = TestEnv::fresh();

        // When installing defaults.
        let report = env.run(false);

        // Then some reference nested beneath a skill directory lands under the
        // skills root at its own nested relative path.
        let nested = report
            .outcomes
            .iter()
            .map(InstallOutcome::path)
            .find(|path| {
                path.starts_with(env.destinations.skills())
                    && path
                        .strip_prefix(env.destinations.skills())
                        .is_ok_and(|rest| rest.components().count() > 2)
            })
            .expect("a skill reference nested under skills/<name>/");
        assert!(nested.is_file());
    }

    /// Every installed payload is byte-identical to its source file under
    /// `res/` — the embedding is verbatim, whatever the file's extension.
    #[rstest::rstest]
    #[test]
    fn installed_payloads_are_byte_identical_to_their_sources() {
        // Given a fresh set of destinations and the on-disk `res/` tree.
        let env = TestEnv::fresh();
        let res = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../res");

        // When installing defaults.
        let report = env.run(false);

        // Then every installed file's bytes match the resource it came from.
        let roots = [
            ("themes", env.destinations.themes()),
            ("personas", env.destinations.personas()),
            ("prompts", env.destinations.prompts()),
            ("skills", env.destinations.skills()),
        ];
        for outcome in &report.outcomes {
            let installed = outcome.path();
            let (kind_dir, under_root) = roots
                .iter()
                .find_map(|(kind_dir, root)| {
                    installed
                        .strip_prefix(root)
                        .ok()
                        .map(|rest| (*kind_dir, rest))
                })
                .unwrap_or_else(|| {
                    panic!("{} is not under any destination root", installed.display())
                });
            let source = res.join(kind_dir).join(under_root);

            assert_eq!(
                std::fs::read(installed).expect("read installed"),
                std::fs::read(&source).expect("read source"),
                "{} was not installed verbatim from {}",
                installed.display(),
                source.display()
            );
        }
    }

    /// The router's `references/*.md` links resolve: every reference the
    /// installed `jinn-usage` SKILL.md points at is installed alongside it.
    #[rstest::rstest]
    #[test]
    fn jinn_usage_router_links_resolve_to_installed_references() {
        // Given a fresh install with a nested skill on disk.
        let env = TestEnv::fresh();
        let report = env.run(false);
        let usage_dir = env.destinations.skills().join("jinn-usage");

        // When extracting the installed router's `references/*.md` links.
        let body = std::fs::read_to_string(usage_dir.join("SKILL.md")).expect("read SKILL.md");
        let linked: Vec<&str> = body
            .lines()
            .filter_map(|line| line.split("references/").nth(1))
            .filter_map(|rest| {
                let end = rest.find('`')?;
                let name = rest.get(..end)?;
                name.to_ascii_lowercase().ends_with(".md").then_some(name)
            })
            .collect();

        // Then the router links at least the core references.
        assert!(
            linked.len() >= 8,
            "expected the router to link its reference files, found {linked:?}"
        );
        // And every linked reference was installed next to it.
        for name in linked {
            assert!(
                usage_dir.join("references").join(name).is_file(),
                "SKILL.md links references/{name} but it was not installed"
            );
        }
        // And the install reported the router itself.
        assert!(
            report
                .outcomes
                .iter()
                .map(InstallOutcome::path)
                .any(|p| p == usage_dir.join("SKILL.md").as_path())
        );
    }

    /// The installed jinn-usage skill parses through the real skill scanner:
    /// valid frontmatter, a name matching its directory, and a
    /// non-empty description (the trigger surface agents match on).
    #[rstest::rstest]
    #[test]
    fn jinn_usage_installs_as_a_discoverable_skill() {
        // Given a fresh install.
        let env = TestEnv::fresh();
        env.run(false);

        // When scanning the skills destination.
        let skills = jinn_skills::scan_skills(env.destinations.skills());

        // Then jinn-usage is discovered by name with a usable description.
        let usage = skills
            .iter()
            .find(|s| s.name == "jinn-usage")
            .expect("jinn-usage must be discovered by the skill scanner");
        assert_ne!(usage.description.trim(), "");
        // And its base_dir holds the router's reference files.
        assert!(usage.base_dir.join("references").is_dir());
    }
}
