//! Cross-platform shell resolution for command execution.
//!
//! jinn runs lifecycle (setup/teardown) commands, tool commands (`bash`,
//! `powershell`), and the interactive terminal through a shell. Which
//! shell to use differs by platform:
//!
//! - **Unix** — `/bin/bash`, or bare `bash` via `PATH`. The bash tool
//!   promises bash syntax (brace expansion, `$$`), and `$SHELL` may be
//!   fish, dash, or zsh, so the environment hint is deliberately ignored.
//! - **Windows** — a POSIX shell is never on `PATH` under its own name:
//!   bare `bash` resolves to `C:\Windows\System32\bash.exe`, the WSL
//!   launcher, which routes commands into a Linux VM and breaks native
//!   Windows tooling (cargo produces Linux binaries, process trees become
//!   invisible to the Windows kill path). So Git Bash is preferred when
//!   installed, and there is **no** WSL fallback on any code path.
//!
//! The environment is probed once, at launch: [`Shells::resolve`] builds
//! the candidate chains and picks the program each one names, and every
//! later read is a lookup in the already-resolved value. A user names
//! their own executables through the `[shell]` section of `jinn.toml` —
//! see [`ShellPathKey`] for the two keys and what they mean.
//!
//! [`Shells`] is the resolved value, and it is the one thing every
//! consumer reads: the bash tool, the PowerShell tool, the PTY, and the
//! session-lifecycle command runner.

// The candidate chains build `ShellCandidate::Path` values from platform
// literals and bare-name joins onto bases already typed `Absolute` (or
// `%LOCALAPPDATA%`): neither construction can introduce a relative segment
// or a drive prefix, so the typed constructions are infallible by
// construction; the panics guard programmer error, not runtime state, and
// the invariant is documented here once for the family.
#![allow(
    clippy::expect_used,
    clippy::missing_panics_doc,
    reason = "literal candidate paths and bare-name joins on typed absolute bases are infallible by construction"
)]

use std::path::Path;

use marked_path::{Absolute, MarkedPathAccess, MarkedPathBuf};
use wherror::Error;

/// The platform a resolution chain is built for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    /// Windows.
    Windows,
    /// Unix — Linux, macOS, and the BSDs.
    Unix,
}

impl Platform {
    /// The platform this build runs on.
    #[must_use]
    pub const fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else {
            Self::Unix
        }
    }
}

/// Which configured shell path a resolution is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellPathKey {
    /// The POSIX shell — `[shell] bash_path`.
    Bash,
    /// The PowerShell executable — `[shell] powershell_path`.
    PowerShell,
}

impl ShellPathKey {
    /// The `jinn.toml` key this path is configured under.
    #[must_use]
    pub const fn config_key(self) -> &'static str {
        match self {
            Self::Bash => "shell.bash_path",
            Self::PowerShell => "shell.powershell_path",
        }
    }
}

/// Why a shell could not be resolved.
#[derive(Debug, Clone, Error)]
pub enum ShellError {
    /// The configured path does not name an existing file on this machine.
    #[error("{key} not found at {path}")]
    ConfiguredPathMissing {
        /// The `jinn.toml` key that carries the path.
        key: &'static str,
        /// The constructed path that was not found.
        path: String,
    },

    /// The configured path is not absolute.
    #[error("{key} is configured to \"{path}\", which is not an absolute path")]
    ConfiguredPathNotAbsolute {
        /// The `jinn.toml` key that carries the path.
        key: &'static str,
        /// The path the key carries.
        path: String,
    },

    /// No candidate named a usable shell.
    #[error("no {kind} was found. Searched: {searched}. {hint}")]
    NotFound {
        /// What was being looked for.
        kind: &'static str,
        /// Every location that was searched, in order.
        searched: String,
        /// What the user can do about it.
        hint: &'static str,
    },
}

impl ShellError {
    /// Whether the failure is the user's configuration rather than the
    /// machine's state.
    ///
    /// A configured path that cannot be used — not absolute here, or
    /// naming no file — is a typo the user wants to hear about at launch.
    /// A shell that is simply not installed is a fact about the machine,
    /// reported at the point of use.
    #[must_use]
    pub const fn is_configuration_error(&self) -> bool {
        matches!(
            self,
            Self::ConfiguredPathMissing { .. } | Self::ConfiguredPathNotAbsolute { .. }
        )
    }
}

/// A shell jinn can run commands through, named concretely.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedShell {
    /// The program to spawn — an absolute path to the executable.
    program: String,
}

impl ResolvedShell {
    /// The program to spawn.
    #[must_use]
    pub fn program(&self) -> &str {
        &self.program
    }
}

/// The shells jinn runs commands through, resolved once at launch.
///
/// The value carries the *outcome* of the resolution, not just a path: a
/// machine with no POSIX shell is a state the consumer has to be able to
/// report, not a crash. The bash tool turns an absent shell into an
/// error the model reads; the session-lifecycle runner turns it into a
/// failed lifecycle command.
#[derive(Debug, Clone)]
pub struct Shells {
    bash: Result<ResolvedShell, ShellError>,
    powershell: Result<ResolvedShell, ShellError>,
}

impl Shells {
    /// Resolves both shells from the user's configuration and this machine.
    ///
    /// A configured path is an all-or-nothing promise: it must construct as
    /// an absolute path on this machine and name an existing file, with no
    /// exception for a path written for another platform, or the
    /// resolution is the configuration error the launch aborts on. Only an
    /// unset key is auto-detected.
    #[must_use]
    pub fn resolve(configured_bash: Option<&str>, configured_powershell: Option<&str>) -> Self {
        Self {
            bash: resolve_shell(configured_bash),
            powershell: resolve_powershell(configured_powershell),
        }
    }

    /// The POSIX shell, or the reason there is none.
    ///
    /// # Errors
    ///
    /// Returns the error the resolution produced: a configured path that
    /// names no file, a configured path that is not absolute, or the
    /// absence of any usable shell on this machine.
    pub fn bash(&self) -> Result<&ResolvedShell, &ShellError> {
        self.bash.as_ref()
    }

    /// The PowerShell executable, or the reason there is none.
    ///
    /// A machine with no PowerShell is a fact about the machine, not a
    /// configuration error: the `powershell` tool reports it when the
    /// model reaches for it, and the launch is not stopped.
    ///
    /// # Errors
    ///
    /// Returns the error the resolution produced: a configured path that
    /// names no file, a configured path that is not absolute, or the
    /// absence of PowerShell on this machine.
    pub fn powershell(&self) -> Result<&ResolvedShell, &ShellError> {
        self.powershell.as_ref()
    }
}

impl Default for Shells {
    /// Auto-detects the shells, ignoring any user configuration.
    fn default() -> Self {
        Self::resolve(None, None)
    }
}

/// One candidate in a shell resolution chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellCandidate {
    /// A concrete path — usable when the file exists.
    Path(MarkedPathBuf<Absolute>),
    /// A bare command name, resolved against `PATH`.
    Command(String),
}

/// The candidate chain for the POSIX shell on `platform`.
///
/// The Windows chain never offers the bare name `bash`: that name
/// resolves to the WSL launcher, which would build Linux binaries inside
/// a VM. `local_app_data` is the user's `%LOCALAPPDATA%` — the third
/// standard Git for Windows install location — and is ignored elsewhere.
#[must_use]
pub fn bash_candidates(
    platform: Platform,
    local_app_data: Option<&MarkedPathBuf<Absolute>>,
) -> Vec<ShellCandidate> {
    match platform {
        Platform::Windows => {
            let mut candidates = vec![
                ShellCandidate::Path(
                    MarkedPathBuf::<Absolute>::new(r"C:\Program Files\Git\bin\bash.exe")
                        .expect("literal candidate path is absolute"),
                ),
                ShellCandidate::Path(
                    MarkedPathBuf::<Absolute>::new(r"C:\Program Files (x86)\Git\bin\bash.exe")
                        .expect("literal candidate path is absolute"),
                ),
            ];
            if let Some(local) = local_app_data {
                candidates.push(ShellCandidate::Path(
                    local
                        .join_all(["Programs", "Git", "bin", "bash.exe"])
                        .expect("name joins keep the base absolute"),
                ));
            }
            candidates
        }
        Platform::Unix => vec![
            ShellCandidate::Path(
                MarkedPathBuf::<Absolute>::new("/bin/bash")
                    .expect("literal candidate path is absolute"),
            ),
            ShellCandidate::Command("bash".to_owned()),
        ],
    }
}

/// The candidate chain for PowerShell on `platform`.
///
/// PowerShell 7 (`pwsh`) comes first: it is the maintained release and
/// the one that ships on a current Windows install. Windows PowerShell 5.1
/// (`powershell.exe`) is the fallback, because it is present on every
/// Windows install.
#[must_use]
pub fn powershell_candidates(platform: Platform) -> Vec<ShellCandidate> {
    match platform {
        Platform::Windows => vec![
            ShellCandidate::Command("pwsh.exe".to_owned()),
            ShellCandidate::Command("powershell.exe".to_owned()),
        ],
        Platform::Unix => vec![ShellCandidate::Command("pwsh".to_owned())],
    }
}

/// The first path in `paths` the predicate accepts.
///
/// The predicate is injected so the precedence can be asserted without a
/// filesystem: a caller that knows which paths are usable hands them in.
/// The resolver passes "is a file", because a directory is not an
/// executable.
#[must_use]
pub fn first_existing<T, F>(paths: &[T], exists: F) -> Option<T>
where
    T: AsRef<Path> + Clone,
    F: Fn(&Path) -> bool,
{
    paths.iter().find(|path| exists(path.as_ref())).cloned()
}

/// Resolves the POSIX shell jinn runs commands through.
///
/// `configured_bash_path` is the user's `[shell] bash_path`, if any. A
/// configured path is used only when it constructs as an absolute path on
/// this machine and names an existing file — there is no exception for a
/// path written for another platform, and a configured path never falls
/// through to auto-detection. Only an unset key reaches the candidate
/// chain.
///
/// # Errors
///
/// Returns [`ShellError::ConfiguredPathMissing`] when the constructed
/// configured path names no file, [`ShellError::ConfiguredPathNotAbsolute`]
/// when it does not construct as absolute on this machine, and
/// [`ShellError::NotFound`] when no candidate yielded a usable shell.
pub fn resolve_shell(configured_bash_path: Option<&str>) -> Result<ResolvedShell, ShellError> {
    if let Some(configured) = configured_bash_path {
        let path = configured_path(ShellPathKey::Bash, configured)?;
        return Ok(ResolvedShell::from_path(&path));
    }
    let platform = Platform::current();
    let candidates = bash_candidates(platform, local_app_data().as_ref());
    resolve_candidates(&candidates, "POSIX shell", bash_missing_hint(platform))
}

/// Resolves the PowerShell executable.
///
/// A configured `powershell_path` obeys the same no-exception rule as
/// `bash_path`: it is used only when it constructs as an absolute path on
/// this machine and names an existing file.
///
/// # Errors
///
/// Returns [`ShellError::ConfiguredPathMissing`] when the constructed
/// configured path names no file, [`ShellError::ConfiguredPathNotAbsolute`]
/// when it does not construct as absolute on this machine, and
/// [`ShellError::NotFound`] when this machine has no PowerShell.
pub fn resolve_powershell(
    configured_powershell_path: Option<&str>,
) -> Result<ResolvedShell, ShellError> {
    if let Some(configured) = configured_powershell_path {
        let path = configured_path(ShellPathKey::PowerShell, configured)?;
        return Ok(ResolvedShell::from_path(&path));
    }
    let platform = Platform::current();
    let candidates = powershell_candidates(platform);
    resolve_candidates(&candidates, "PowerShell", POWERSHELL_MISSING_HINT)
}

/// Resolves a configured path to the file it must name on this machine.
///
/// There is no cross-platform exception. The configured string is
/// constructed as an absolute path on the platform jinn runs on — a
/// relative spelling and a path written for the other OS both fail that
/// construction — and the constructed path must name an existing file.
/// Either failure is a configuration error the launch aborts on.
fn configured_path(key: ShellPathKey, path: &str) -> Result<MarkedPathBuf<Absolute>, ShellError> {
    // The construction is the check: `marked-path` accepts a string only
    // when it is absolute here, and rewrites separators to this
    // platform's own.
    let constructed = MarkedPathBuf::<Absolute>::new(path).map_err(|_error| {
        ShellError::ConfiguredPathNotAbsolute {
            key: key.config_key(),
            path: path.to_owned(),
        }
    })?;
    if constructed.is_file() {
        Ok(constructed)
    } else {
        Err(ShellError::ConfiguredPathMissing {
            key: key.config_key(),
            path: constructed.to_string(),
        })
    }
}

/// Picks the first usable candidate in a chain.
fn resolve_candidates(
    candidates: &[ShellCandidate],
    kind: &'static str,
    hint: &'static str,
) -> Result<ResolvedShell, ShellError> {
    let searched = candidates
        .iter()
        .map(describe_candidate)
        .collect::<Vec<_>>()
        .join(", ");
    let paths = candidate_paths(candidates);
    match first_existing(&paths, |path: &std::path::Path| path.is_file()) {
        Some(path) => Ok(ResolvedShell::from_path(&path)),
        None => Err(ShellError::NotFound {
            kind,
            searched,
            hint,
        }),
    }
}

/// How a candidate is named in a "searched these places" message.
fn describe_candidate(candidate: &ShellCandidate) -> String {
    match candidate {
        ShellCandidate::Path(path) => path.to_string(),
        ShellCandidate::Command(name) => format!("{name} (on PATH)"),
    }
}

/// The concrete paths a candidate chain names, in order.
#[must_use]
pub fn candidate_paths(candidates: &[ShellCandidate]) -> Vec<MarkedPathBuf<Absolute>> {
    candidates
        .iter()
        .filter_map(|candidate| match candidate {
            ShellCandidate::Path(path) => Some(path.clone()),
            ShellCandidate::Command(name) => find_on_path(name),
        })
        .collect()
}

/// The first `PATH` entry that holds a file named `name`.
fn find_on_path(name: &str) -> Option<MarkedPathBuf<Absolute>> {
    let path_env = std::env::var_os("PATH")?;
    let candidate = std::env::split_paths(&path_env)
        .filter(|directory| !directory.as_os_str().is_empty())
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())?;
    MarkedPathBuf::<Absolute>::new(candidate).ok()
}

/// The user's `%LOCALAPPDATA%`, read at resolution time.
fn local_app_data() -> Option<MarkedPathBuf<Absolute>> {
    let raw = std::env::var_os("LOCALAPPDATA")?;
    MarkedPathBuf::<Absolute>::new(raw).ok()
}

/// The guidance shown when no POSIX shell was found.
fn bash_missing_hint(platform: Platform) -> &'static str {
    match platform {
        Platform::Windows => {
            "Install Git for Windows, or set shell.bash_path in jinn.toml to the path of your bash.exe."
        }
        Platform::Unix => {
            "Install bash, or set shell.bash_path in jinn.toml to the path of your bash."
        }
    }
}

/// The guidance shown when no PowerShell was found.
const POWERSHELL_MISSING_HINT: &str = "Install PowerShell, or set shell.powershell_path in jinn.toml to the path of your PowerShell executable.";

/// The arguments a PowerShell command is given, in order.
///
/// `-NoProfile` keeps the user's PowerShell profile out of the run — a
/// profile is arbitrary user code, and a tool call must run the same
/// command the same way every time. `-NonInteractive` keeps PowerShell
/// from blocking on a prompt nobody can answer. `-Command` runs the
/// command that follows it.
///
/// The list lives here rather than in the tool that uses it so the
/// invocation shape is asserted on every platform, not only on the one
/// the tool is registered on.
#[must_use]
pub fn powershell_command_args(command: &str) -> Vec<String> {
    vec![
        "-NoProfile".to_owned(),
        "-NonInteractive".to_owned(),
        "-Command".to_owned(),
        command.to_owned(),
    ]
}

impl ResolvedShell {
    /// Names an already-resolved program.
    fn from_path(path: &MarkedPathBuf<Absolute>) -> Self {
        Self {
            program: path.to_string_lossy().into_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        reason = "test code asserts with expect/panic for clear failure messages"
    )]
    // Probe-file fixtures write through raw std paths: the raw candidate
    // check is the behavior under test.
    #![expect(
        clippy::disallowed_types,
        reason = "test fixtures probe candidate existence through raw std paths; the raw probe is the behavior under test"
    )]
    use super::{
        Platform, ShellCandidate, Shells, bash_candidates, candidate_paths, first_existing,
        powershell_candidates, resolve_candidates, resolve_powershell, resolve_shell,
    };
    use jinn_testutil::volume_root;
    use marked_path::MarkedPathAccess;
    // Only the Windows-gated chain tests construct typed literals.
    #[cfg(windows)]
    use marked_path::{Absolute, MarkedPathBuf};
    use std::path::{Path, PathBuf};

    /// Creates a file that exists, for a candidate to win on.
    ///
    /// The caller keeps the temp dir alive: a dropped dir takes its file
    /// with it, and the candidate would no longer exist by the time it is
    /// tested.
    fn existing_file(dir: &std::path::Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, b"probe").expect("write probe");
        path
    }

    #[rstest::rstest]
    #[test]
    fn first_existing_picks_the_earliest_existing_candidate() {
        // Given two candidate paths where only the second exists.
        let dir = jinn_testutil::tempdir_with_prefix("jinn-test-shell-resolver");
        let existing = existing_file(dir.path.as_path(), "second");
        let missing = volume_root()
            .join_all(["definitely", "not", "here", "shell"])
            .expect("bare-name joins keep the base absolute")
            .as_path()
            .to_path_buf();
        let candidates = vec![missing, existing.clone()];

        // When picking the first existing one.
        let picked = first_existing(&candidates, Path::exists);

        // Then the existing candidate wins.
        assert_eq!(picked, Some(existing));
    }

    #[rstest::rstest]
    #[test]
    fn first_existing_returns_none_when_no_candidate_exists() {
        // Given candidates that name no file.
        let candidates = vec![
            volume_root()
                .join_all(["nope", "one"])
                .expect("bare-name joins keep the base absolute")
                .as_path()
                .to_path_buf(),
            volume_root()
                .join_all(["nope", "two"])
                .expect("bare-name joins keep the base absolute")
                .as_path()
                .to_path_buf(),
        ];

        // When picking the first existing one.
        let picked = first_existing(&candidates, Path::exists);

        // Then there is none.
        assert_eq!(picked, None);
    }

    #[rstest::rstest]
    #[test]
    #[cfg(windows)]
    fn windows_bash_chain_never_offers_the_bare_bash_name() {
        // Given the Windows candidate chain.
        let candidates = bash_candidates(Platform::Windows, None);

        // When looking for a bare command name in it.
        let bare = candidates
            .iter()
            .any(|candidate| matches!(candidate, ShellCandidate::Command(name) if name == "bash"));

        // Then there is none: bare `bash` on Windows is the WSL launcher.
        assert!(
            !bare,
            "the Windows chain must not offer the bare `bash` name: {candidates:?}"
        );
    }

    #[rstest::rstest]
    #[test]
    #[cfg(windows)]
    fn windows_bash_chain_prefers_the_program_files_install() {
        // Given the Windows candidate chain.
        let candidates = bash_candidates(Platform::Windows, None);

        // When reading its first candidate.
        let first = candidates.first().expect("the chain is never empty");

        // Then it is the standard 64-bit Git for Windows install.
        assert_eq!(
            first,
            &ShellCandidate::Path(
                MarkedPathBuf::<Absolute>::new(r"C:\Program Files\Git\bin\bash.exe")
                    .expect("test path"),
            )
        );
    }

    #[rstest::rstest]
    #[test]
    #[cfg(windows)]
    fn windows_bash_chain_includes_the_local_app_data_install() {
        // Given a `%LOCALAPPDATA%` root.
        let local =
            MarkedPathBuf::<Absolute>::new(r"C:\Users\jinn\AppData\Local").expect("test path");

        // When building the Windows candidate chain.
        let candidates = bash_candidates(Platform::Windows, Some(&local));

        // Then the per-user Git install is offered.
        let expected = local
            .join_all(["Programs", "Git", "bin", "bash.exe"])
            .expect("name joins keep the base absolute");
        assert!(
            candidates.contains(&ShellCandidate::Path(expected)),
            "the per-user Git install must be in the chain: {candidates:?}"
        );
    }

    // Gated: the Unix chain is only ever built on Unix, and its leading
    // candidate names the POSIX-rooted `/bin/bash`, which cannot construct
    // on a Windows host (drive-relative there). Building the chain on
    // Windows would panic in production-shaped code, so the assertion runs
    // where the chain itself is reachable.
    #[cfg(unix)]
    #[rstest::rstest]
    #[test]
    fn unix_bash_chain_falls_back_to_the_path_lookup() {
        // Given the Unix candidate chain.
        let candidates = bash_candidates(Platform::Unix, None);

        // When reading its last candidate.
        let last = candidates.last().expect("the chain is never empty");

        // Then it is the bare `bash` name, resolved against PATH.
        assert_eq!(last, &ShellCandidate::Command("bash".to_owned()));
    }

    #[rstest::rstest]
    #[test]
    fn windows_powershell_chain_prefers_pwsh_over_windows_powershell() {
        // Given the Windows PowerShell candidate chain.
        let candidates = powershell_candidates(Platform::Windows);

        // When reading the chain.
        // Then PowerShell 7 comes first and Windows PowerShell second.
        assert_eq!(
            candidates,
            vec![
                ShellCandidate::Command("pwsh.exe".to_owned()),
                ShellCandidate::Command("powershell.exe".to_owned()),
            ]
        );
    }

    #[rstest::rstest]
    #[test]
    fn candidate_paths_keeps_the_chain_order() {
        // Given a chain of two concrete paths.
        let first = volume_root()
            .join_all(["first"])
            .expect("bare-name joins keep the base absolute");
        let second = volume_root()
            .join_all(["second"])
            .expect("bare-name joins keep the base absolute");
        let candidates = vec![
            ShellCandidate::Path(first.clone()),
            ShellCandidate::Path(second.clone()),
        ];

        // When resolving the chain into concrete paths.
        let paths = candidate_paths(&candidates);

        // Then the order is preserved.
        assert_eq!(paths, vec![first, second]);
    }

    #[rstest::rstest]
    #[test]
    fn resolve_shell_uses_a_configured_path_that_exists() {
        // Given a configured bash path that names a real file.
        let dir = jinn_testutil::tempdir_with_prefix("jinn-test-shell-resolver");
        let bash = existing_file(dir.path.as_path(), "bash-exe");
        let configured = bash.to_string_lossy().into_owned();

        // When resolving the shell.
        let resolved = resolve_shell(Some(&configured));

        // Then the configured file is the shell.
        let resolved = resolved.expect("the configured path exists");
        assert_eq!(Path::new(resolved.program()), bash.as_path());
    }

    #[rstest::rstest]
    #[test]
    fn resolve_shell_reports_a_configured_path_that_does_not_exist() {
        // Given an absolute configured bash path that names no file.
        let dir = jinn_testutil::tempdir_with_prefix("jinn-test-shell-resolver");
        let missing = dir
            .path
            .join_all(["no-such-dir", "bash"])
            .expect("name joins keep the base absolute");
        let configured = missing.to_string_lossy().into_owned();

        // When resolving the shell.
        let result = resolve_shell(Some(&configured));

        // Then the failure is "not found at" the constructed path.
        let Err(error) = result else {
            panic!("a missing configured path must not resolve");
        };
        assert_eq!(
            error.to_string(),
            format!("shell.bash_path not found at {configured}")
        );
    }

    #[rstest::rstest]
    #[test]
    fn a_missing_configured_path_is_a_configuration_error() {
        // Given an absolute configured bash path that names no file.
        let dir = jinn_testutil::tempdir_with_prefix("jinn-test-shell-resolver");
        let missing = dir
            .path
            .join_all(["no-such-dir", "bash"])
            .expect("name joins keep the base absolute");
        let configured = missing.to_string_lossy().into_owned();

        // When resolving the shell.
        let result = resolve_shell(Some(&configured));

        // Then the failure is the user's configuration, not the machine.
        let Err(error) = result else {
            panic!("a missing configured path must not resolve");
        };
        assert!(error.is_configuration_error());
    }

    #[rstest::rstest]
    #[test]
    fn resolve_shell_reports_a_relative_configured_path() {
        // Given a configured bash path that is not absolute.
        // When resolving the shell.
        let result = resolve_shell(Some("bash.exe"));

        // Then the failure names the key.
        let Err(error) = result else {
            panic!("a relative configured path must not resolve");
        };
        assert!(
            error.to_string().contains("shell.bash_path"),
            "the failure must name the key: {error}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn a_configured_path_for_the_other_platform_aborts_the_launch() {
        // Given a configured path written for the other platform.
        let configured = if cfg!(windows) {
            "/usr/bin/bash"
        } else {
            r"C:\Program Files\Git\bin\bash.exe"
        };

        // When resolving the shell.
        let result = resolve_shell(Some(configured));

        // Then the path is not this machine's to use: the resolution is
        // the configuration error the launch aborts on, naming the key.
        let Err(error) = result else {
            panic!("a path for the other platform must not resolve");
        };
        assert!(
            error.is_configuration_error(),
            "a path for the other platform must be a configuration error: {error}"
        );
        assert!(
            error.to_string().contains("shell.bash_path"),
            "the failure must name the key: {error}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn a_failed_auto_detection_names_every_place_searched() {
        // Given a candidate chain where nothing exists.
        let nowhere = volume_root()
            .join_all(["nowhere", "one"])
            .expect("bare-name joins keep the base absolute");
        let candidates = vec![
            ShellCandidate::Path(nowhere.clone()),
            ShellCandidate::Command("nowhere-two".to_owned()),
        ];

        // When resolving it.
        let result = resolve_candidates(&candidates, "POSIX shell", "install one");

        // Then the failure names each place that was searched.
        let Err(error) = result else {
            panic!("a chain with nothing usable must not resolve");
        };
        let message = error.to_string();
        let needle = nowhere.to_string();
        assert!(
            message.contains(&needle),
            "the failure must name the searched path: {message}"
        );
        assert!(
            message.contains("nowhere-two"),
            "the failure must name the searched command: {message}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn a_configured_path_that_is_a_directory_is_rejected() {
        // Given a configured path that names a directory.
        let dir = jinn_testutil::tempdir_with_prefix("jinn-test-shell-resolver");
        let configured = dir.path.to_string();

        // When resolving the shell.
        let result = resolve_shell(Some(&configured));

        // Then the directory is not accepted as an executable.
        let Err(error) = result else {
            panic!("a directory must not resolve as a shell");
        };
        assert!(
            error.to_string().contains("shell.bash_path"),
            "the failure must name the key: {error}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn a_powershell_command_runs_with_no_profile_and_non_interactively() {
        // Given a PowerShell command.
        // When building the argument list.
        let args = super::powershell_command_args("Get-ChildItem");

        // Then the profile is skipped, the shell never prompts, and the
        // command is the last argument.
        assert_eq!(
            args,
            vec![
                "-NoProfile".to_owned(),
                "-NonInteractive".to_owned(),
                "-Command".to_owned(),
                "Get-ChildItem".to_owned(),
            ]
        );
    }

    #[rstest::rstest]
    #[test]
    fn resolve_powershell_reports_a_configured_path_that_does_not_exist() {
        // Given an absolute configured PowerShell path that names no file.
        let dir = jinn_testutil::tempdir_with_prefix("jinn-test-shell-resolver");
        let missing = dir
            .path
            .join_all(["no-such-dir", "pwsh"])
            .expect("name joins keep the base absolute");
        let configured = missing.to_string_lossy().into_owned();

        // When resolving PowerShell.
        let result = resolve_powershell(Some(&configured));

        // Then the failure is "not found at" the constructed path.
        let Err(error) = result else {
            panic!("a missing configured path must not resolve");
        };
        assert_eq!(
            error.to_string(),
            format!("shell.powershell_path not found at {configured}")
        );
    }

    #[rstest::rstest]
    #[test]
    fn shells_reports_a_missing_bash_as_a_resolved_error() {
        // Given a configured bash path that names no file.
        // When resolving the shells.
        let shells = Shells::resolve(Some("definitely-not-here/bash.exe"), None);

        // Then the bash shell is an error naming the key.
        let Err(error) = shells.bash() else {
            panic!("a missing configured path must not resolve");
        };
        assert!(error.to_string().contains("shell.bash_path"));
    }

    #[rstest::rstest]
    #[test]
    fn shells_reports_a_missing_powershell_as_a_resolved_error() {
        // Given a configured PowerShell path that names no file.
        // When resolving the shells.
        let shells = Shells::resolve(None, Some("definitely-not-here/pwsh.exe"));

        // Then the PowerShell shell is an error naming the key.
        let Err(error) = shells.powershell() else {
            panic!("a missing configured path must not resolve");
        };
        assert!(error.to_string().contains("shell.powershell_path"));
    }

    #[cfg(not(windows))]
    #[rstest::rstest]
    #[test]
    fn a_configured_path_is_used_verbatim_on_unix() {
        // Given a configured path whose backslashes are ordinary characters.
        let dir = jinn_testutil::tempdir_with_prefix("jinn-test-shell-resolver");
        let bash = dir.path.join("bash").expect("temp root joins a bare name");
        std::fs::write(&bash, b"probe").expect("write probe");
        let configured = bash.to_string_lossy().into_owned();

        // When resolving the shell.
        let resolved = resolve_shell(Some(&configured));

        // Then the very same path is used, separators untouched.
        let resolved = resolved.expect("the configured path exists");
        assert_eq!(Path::new(resolved.program()), bash.as_path());
    }

    #[cfg(windows)]
    #[rstest::rstest]
    #[test]
    fn a_configured_path_is_normalized_to_native_separators() {
        // Given a configured path written with forward slashes. It is built
        // from the volume root — absolute on this machine, naming no file —
        // so its existence is never a fact about the dev box.
        let native = volume_root()
            .join_all(["nowhere", "bash.exe"])
            .expect("bare-name joins keep the base absolute");
        let configured = native.as_path().to_string_lossy().replace('\\', "/");

        // When resolving the shell.
        let result = resolve_shell(Some(&configured));

        // Then the path is looked up in its native spelling, and the
        // failure names it in the constructed, native spelling.
        let Err(error) = result else {
            panic!("the path does not exist on this machine");
        };
        assert!(
            matches!(error, super::ShellError::ConfiguredPathMissing { .. }),
            "the failure must be a missing configured path: {error}"
        );
        assert!(
            error
                .to_string()
                .contains(native.as_path().to_string_lossy().as_ref()),
            "the failure must name the native-spelling path: {error}"
        );
    }
}
