//! Cross-platform POSIX-shell resolution for command execution.
//!
//! jinn runs lifecycle (setup/teardown) and tool (bash) commands through a
//! POSIX shell. Which shell to use differs by platform:
//!
//! - **Unix** — bare `bash` via `PATH`. The bash tool promises bash syntax
//!   (brace expansion, `$$`), and `$SHELL` may be fish, dash, or zsh, so the
//!   environment hint is deliberately ignored there.
//! - **Windows** — a POSIX shell is never on `PATH` under its own name:
//!   bare `bash` resolves to `C:\Windows\System32\bash.exe`, the WSL
//!   launcher, which routes commands into a Linux VM and breaks native
//!   Windows tooling (cargo produces Linux binaries, process trees become
//!   invisible to the Windows kill path). So Git Bash is preferred when
//!   installed, `$SHELL` (as set by MSYS environments) next, bare `bash`
//!   last.
//!
//! [`resolve_shell`] is the one shared resolver both consumers call, so the
//! precedence cannot drift between them.

use std::path::{Path, PathBuf};

/// The canonical Git Bash install location, the preferred Windows shell.
#[cfg(windows)]
const GIT_BASH: &str = r"C:\Program Files\Git\bin\bash.exe";

/// Resolves the shell to run commands through on this platform.
///
/// Returns a shell that is either a proven-existing file (a candidate that
/// was checked on disk) or a bare command name for `PATH` resolution — the
/// caller spawns it directly. See the [module docs](self) for the platform
/// precedence; the decision itself is [`resolve_shell_from`], which is pure
/// and unit-tested.
#[must_use]
pub fn resolve_shell() -> String {
    #[cfg(windows)]
    {
        let git_bash = Some(PathBuf::from(GIT_BASH));
        let env_shell = std::env::var("SHELL").ok().map(PathBuf::from);
        resolve_shell_from([git_bash, env_shell, None])
    }
    #[cfg(not(windows))]
    {
        resolve_shell_from([None::<PathBuf>; 0])
    }
}

/// Pure precedence core of [`resolve_shell`]: picks the first *usable*
/// candidate.
///
/// A candidate is usable when its `Some(path)` exists on disk (an absolute
/// hint like the Git Bash install location or `$SHELL` must be real before
/// it wins), or when it is `None`, which stands for the bare `bash` name —
/// left to `PATH` resolution at spawn time, so it is always usable. The
/// ordering the callers pass in encodes the platform precedence:
///
/// - Windows: Git Bash path, `$SHELL`, then bare `bash`.
/// - Unix: bare `bash` only (the single `None` candidate).
///
/// Duplicated candidates are skipped, so a `$SHELL` that already points at
/// Git Bash resolves identically whether it or the install path won.
#[must_use]
pub fn resolve_shell_from<I>(candidates: I) -> String
where
    I: IntoIterator,
    I::Item: Into<Option<PathBuf>>,
{
    let mut seen: Vec<String> = Vec::new();
    for candidate in candidates {
        match candidate.into() {
            Some(path) => {
                let text = path.to_string_lossy().into_owned();
                if seen.iter().any(|prior| prior == &text) {
                    continue;
                }
                if Path::new(&path).exists() {
                    return text;
                }
                seen.push(text);
            }
            // Bare name for `PATH` resolution — the universal fallback.
            None => return "bash".to_owned(),
        }
    }
    "bash".to_owned()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, reason = "test code")]
    use super::{resolve_shell, resolve_shell_from};
    use std::path::PathBuf;

    #[rstest::rstest]
    #[test]
    fn resolve_shell_returns_a_nonempty_command() {
        // Given any platform's candidate set.
        // When resolving the shell.
        let shell = resolve_shell();

        // Then the result names a shell to spawn.
        assert!(!shell.is_empty(), "resolver must always return a shell");
    }

    #[rstest::rstest]
    #[test]
    fn nonexistent_first_candidate_falls_through_to_bare_bash() {
        // Given a candidate list whose only entry is a path that does not exist.
        let missing = PathBuf::from(r"Z:\nowhere\bash.exe");

        // When resolving from that list.
        let shell = resolve_shell_from([Some(missing), None]);

        // Then the bare PATH-resolved name is the fallback.
        assert_eq!(shell, "bash");
    }

    #[rstest::rstest]
    #[test]
    fn existing_candidate_wins_over_later_candidates() {
        // Given a list where an existing file comes before the bare fallback.
        let existing = std::env::temp_dir().join("shell-resolver-probe");
        std::fs::write(&existing, b"probe").expect("write probe");

        // When resolving from that list.
        let shell = resolve_shell_from([Some(existing.clone()), None]);

        // Then the earlier existing candidate wins.
        assert_eq!(shell, existing.to_string_lossy());
        std::fs::remove_file(&existing).expect("remove probe");
    }

    #[rstest::rstest]
    #[test]
    fn empty_candidate_list_falls_back_to_bare_bash() {
        // Given no candidates at all.
        // When resolving from the empty list.
        let shell = resolve_shell_from(Vec::<Option<PathBuf>>::new());

        // Then the bare PATH-resolved name is the answer.
        assert_eq!(shell, "bash");
    }

    #[rstest::rstest]
    #[test]
    fn duplicate_existing_candidate_is_not_double_matched() {
        // Given the same existing file twice around a bare fallback.
        let existing = std::env::temp_dir().join("shell-resolver-dup");
        std::fs::write(&existing, b"probe").expect("write probe");

        // When resolving from the duplicated list.
        let shell = resolve_shell_from([Some(existing.clone()), Some(existing.clone()), None]);

        // Then the existing candidate still resolves once, to itself.
        assert_eq!(shell, existing.to_string_lossy());
        std::fs::remove_file(&existing).expect("remove probe");
    }

    #[cfg(windows)]
    mod windows {
        use super::super::{GIT_BASH, resolve_shell, resolve_shell_from};
        use std::path::PathBuf;

        #[rstest::rstest]
        #[test]
        fn git_bash_precedes_the_bare_bash_fallback() {
            // Given the real Git Bash install (verified present on this machine).
            // When resolving with Git Bash ahead of the bare fallback.
            let shell = resolve_shell_from([Some(PathBuf::from(GIT_BASH)), None]);

            // Then the Git Bash path wins when it exists, and otherwise the
            // fallback does — never a different candidate.
            if PathBuf::from(GIT_BASH).exists() {
                assert_eq!(shell, GIT_BASH);
            } else {
                assert_eq!(shell, "bash");
            }
        }

        #[rstest::rstest]
        #[test]
        fn resolve_shell_prefers_git_bash_when_installed() {
            // Given a machine with the canonical Git Bash install.
            if !PathBuf::from(GIT_BASH).exists() {
                return;
            }

            // When resolving the shell for this platform.
            let shell = resolve_shell();

            // Then the Git Bash path is chosen over any bare name.
            assert_eq!(shell, GIT_BASH);
        }
    }
}
