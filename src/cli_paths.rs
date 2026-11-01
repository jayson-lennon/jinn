//! Resolution of user-typed path flags at the CLI boundary.
//!
//! `--log-file`, `--db-path`, and `--dump-requests` all take a path the user
//! typed in a shell. Three shapes reach the resolver: an absolute path (taken
//! verbatim), a tilde path (expanded against the home directory), and a
//! relative path (joined against the process cwd, the same rule every
//! relative CLI path in jinn follows). Joins normalize lexically, so
//! `./foo.log` lands as `<cwd>/foo.log` and `~/x/../y.log` as `<home>/y.log`.
//!
//! Every failure is a clean usage error naming the flag and the value the
//! user typed — never a panic. The home directory is injected by the caller
//! (`AppPaths::home_dir()`); the cwd is read once, only when a relative value
//! actually needs it, and can be replaced through [`resolve_cli_path_in`] so
//! tests never touch the process cwd.
//!
//! This module is the seam where raw CLI values are re-typed: the `raw`
//! parameters are the user's own text, read as borrowed `&Path`, and the
//! return values are `MarkedPathBuf<Absolute>` by construction.
#![expect(
    clippy::disallowed_types,
    reason = "argv boundary: this module consumes raw user-typed flag values (and the raw process-cwd read) and is where they are re-typed into MarkedPathBuf<Absolute>"
)]

use std::path::Path;
use std::path::PathBuf;

use error_stack::Report;
use error_stack::ResultExt;
use marked_path::Absolute;
use marked_path::MarkedPathBuf;
use wherror::Error;

/// A CLI path flag carried a value that could not be resolved to an
/// absolute path.
///
/// The flag name and the offending value ride the report as attachments, so
/// the rendered error names exactly what the user typed.
#[derive(Debug, Error)]
#[error(debug)]
pub struct CliPathError;

/// Resolves a user-typed CLI path value against the process context.
///
/// A leading `~` (alone or before a separator) expands against `home`;
/// otherwise an absolute value is taken verbatim; otherwise the value joins
/// the process cwd and the join normalizes lexically. `flag` appears in the
/// error attachments, so pass the flag exactly as the user spells it
/// (e.g. `--log-file`).
///
/// # Errors
///
/// Returns an error naming the flag and the raw value when the process cwd
/// is unavailable (a relative value needs it) or when the value cannot be an
/// absolute path.
pub fn resolve_cli_path(
    flag: &str,
    raw: &Path,
    home: &MarkedPathBuf<Absolute>,
) -> Result<MarkedPathBuf<Absolute>, Report<CliPathError>> {
    resolve_cli_path_in(flag, raw, home, std::env::current_dir)
}

/// [`resolve_cli_path`] with the cwd read injected.
///
/// Production callers use [`resolve_cli_path`]. Tests inject a supplier so a
/// relative value resolves against a chosen directory, or so a failing
/// supplier pins the cwd-unavailable error — without `set_current_dir`,
/// which is process-global and races the suite.
///
/// # Errors
///
/// Same contract as [`resolve_cli_path`].
pub fn resolve_cli_path_in<F>(
    flag: &str,
    raw: &Path,
    home: &MarkedPathBuf<Absolute>,
    current_dir: F,
) -> Result<MarkedPathBuf<Absolute>, Report<CliPathError>>
where
    F: FnOnce() -> std::io::Result<PathBuf>,
{
    // A tilde value resolves against home and never needs the cwd.
    if let Some(expanded) = tilde_expansion(raw, home) {
        return expanded
            .map_err(|_| CliPathError)
            .attach_with(|| format!("{flag} tilde value `{}` is unusable", raw.display()))
            .map(|typed| typed.normalize_lexical());
    }

    // An absolute value is taken verbatim — the components the user typed
    // (`.`/`..` included) are preserved, and the cwd is never consulted.
    if raw.is_absolute() {
        return MarkedPathBuf::<Absolute>::new(raw.to_owned())
            .map_err(|_| CliPathError)
            .attach_with(|| format!("{flag} value `{}` is not absolute", raw.display()));
    }

    // A relative value joins the process cwd, then normalizes so the
    // user-typed shorthand (`./x`, `a/../x`) collapses to its meaning.
    let joined = current_dir()
        .change_context(CliPathError)
        .attach_with(|| {
            format!(
                "cannot resolve the relative `{flag}` value `{}`: process cwd is unavailable",
                raw.display()
            )
        })?
        .join(raw);
    MarkedPathBuf::<Absolute>::new(joined)
        .map_err(|_| CliPathError)
        .attach_with(|| format!("{flag} value `{}` is not absolute", raw.display()))
        .map(|typed| typed.normalize_lexical())
}

/// Expands a leading `~` in `raw` against `home`.
///
/// Bare `~` is the home itself; `~` followed by a platform separator
/// (`~/` always, `~\` where `\` is a separator) joins the remainder onto
/// `home`. `~name` is *not* a tilde value — it falls through as a literal
/// relative path, matching the cwd popup's resolver. Non-UTF-8 values are
/// never tilde values.
fn tilde_expansion(
    raw: &Path,
    home: &MarkedPathBuf<Absolute>,
) -> Option<Result<MarkedPathBuf<Absolute>, marked_path::PathError>> {
    let text = raw.to_str()?;
    if text == "~" {
        return Some(Ok(home.clone()));
    }
    let rest = strip_tilde_prefix(text)?;
    Some(home.join(rest))
}

/// Strips the `~` marker plus its following separator from `text`.
///
/// [`std::path::is_separator`] is platform-aware: on Unix only `/` counts,
/// so `~\foo` stays a literal path; on Windows both `/` and `\` count.
fn strip_tilde_prefix(text: &str) -> Option<&str> {
    let rest = text.strip_prefix('~')?;
    let mut chars = rest.chars();
    match chars.next() {
        Some(sep) if std::path::is_separator(sep) => Some(chars.as_str()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "test code")]

    use marked_path::Absolute;
    use marked_path::MarkedPathAccess;
    use marked_path::MarkedPathBuf;
    use std::path::PathBuf;

    use super::resolve_cli_path_in;

    /// A cwd supplier that always fails, for pinning the error path and for
    /// proving a branch never consults the cwd.
    fn no_cwd() -> std::io::Result<PathBuf> {
        Err(std::io::Error::other("cwd unavailable"))
    }

    /// A cwd supplier pinned to `dir`.
    fn cwd_at(dir: &MarkedPathBuf<Absolute>) -> impl Fn() -> std::io::Result<PathBuf> {
        let dir = dir.as_path().to_path_buf();
        move || Ok(dir.clone())
    }

    #[rstest::rstest]
    #[test]
    fn relative_value_resolves_against_the_injected_cwd() {
        // Given a relative flag value and an injected cwd.
        let dir = jinn_testutil::tempdir();
        let home = home_at(&dir, "home");
        let cwd = home_at(&dir, "work");

        // When resolving `foo.log`.
        let resolved = resolve_cli_path_in(
            "--log-file",
            std::path::Path::new("foo.log"),
            &home,
            cwd_at(&cwd),
        )
        .expect("relative value resolves");

        // Then it lands directly under the injected cwd.
        let expected = cwd.join("foo.log").expect("cwd joins a bare filename");
        assert_eq!(resolved, expected);
    }

    #[rstest::rstest]
    #[test]
    fn dot_components_collapse_after_the_cwd_join() {
        // Given a relative flag value written with an explicit `.`.
        let dir = jinn_testutil::tempdir();
        let home = home_at(&dir, "home");
        let cwd = home_at(&dir, "work");

        // When resolving `./foo.log`.
        let resolved = resolve_cli_path_in(
            "--log-file",
            std::path::Path::new("./foo.log"),
            &home,
            cwd_at(&cwd),
        )
        .expect("relative value resolves");

        // Then the join collapses to the cwd-joined form.
        let expected = cwd.join("foo.log").expect("cwd joins a bare filename");
        assert_eq!(resolved, expected);
    }

    #[rstest::rstest]
    #[test]
    fn parent_components_resolve_after_the_cwd_join() {
        // Given a relative flag value that climbs and comes back.
        let dir = jinn_testutil::tempdir();
        let home = home_at(&dir, "home");
        let cwd = home_at(&dir, "work");

        // When resolving `a/../foo.log`.
        let resolved = resolve_cli_path_in(
            "--log-file",
            std::path::Path::new("a/../foo.log"),
            &home,
            cwd_at(&cwd),
        )
        .expect("relative value resolves");

        // Then the `..` resolves lexically against the joined cwd.
        let expected = cwd.join("foo.log").expect("cwd joins a bare filename");
        assert_eq!(resolved, expected);
    }

    #[rstest::rstest]
    #[test]
    fn bare_tilde_resolves_against_home_without_the_cwd() {
        // Given the tilde by itself and a cwd supplier that always fails.
        let dir = jinn_testutil::tempdir();
        let home = home_at(&dir, "home");

        // When resolving `~`.
        let resolved = resolve_cli_path_in("--log-file", std::path::Path::new("~"), &home, no_cwd)
            .expect("bare tilde resolves");

        // Then it is the home directory, and the cwd was never read.
        assert_eq!(resolved, home);
    }

    #[rstest::rstest]
    #[test]
    fn tilde_slash_value_resolves_against_home() {
        // Given a `~/`-prefixed flag value.
        let dir = jinn_testutil::tempdir();
        let home = home_at(&dir, "home");

        // When resolving `~/x.log`.
        let resolved =
            resolve_cli_path_in("--log-file", std::path::Path::new("~/x.log"), &home, no_cwd)
                .expect("tilde value resolves");

        // Then the remainder joins the home directory.
        let expected = home.join("x.log").expect("home joins a bare filename");
        assert_eq!(resolved, expected);
    }

    #[rstest::rstest]
    #[test]
    fn tilde_parent_components_resolve_against_home() {
        // Given a `~/`-prefixed value that climbs and comes back.
        let dir = jinn_testutil::tempdir();
        let home = home_at(&dir, "home");

        // When resolving `~/x/../y.log`.
        let resolved = resolve_cli_path_in(
            "--log-file",
            std::path::Path::new("~/x/../y.log"),
            &home,
            no_cwd,
        )
        .expect("tilde value resolves");

        // Then the join normalizes to the home-joined form.
        let expected = home.join("y.log").expect("home joins a bare filename");
        assert_eq!(resolved, expected);
    }

    #[rstest::rstest]
    #[test]
    fn tilde_name_without_separator_is_a_literal_relative_path() {
        // Given a `~name` value with no separator after the tilde.
        let dir = jinn_testutil::tempdir();
        let home = home_at(&dir, "home");
        let cwd = home_at(&dir, "work");

        // When resolving `~foo.log`.
        let resolved = resolve_cli_path_in(
            "--log-file",
            std::path::Path::new("~foo.log"),
            &home,
            cwd_at(&cwd),
        )
        .expect("literal value resolves");

        // Then it is an ordinary relative path against the cwd, not a
        // home lookup.
        let expected = cwd.join("~foo.log").expect("cwd joins a bare filename");
        assert_eq!(resolved, expected);
    }

    #[cfg(unix)]
    #[rstest::rstest]
    #[test]
    fn tilde_backslash_value_is_literal_on_unix() {
        // Given a `~\`-prefixed value on a platform where `\` is not a
        // separator.
        let dir = jinn_testutil::tempdir();
        let home = home_at(&dir, "home");
        let cwd = home_at(&dir, "work");

        // When resolving `~\x.log`.
        let resolved = resolve_cli_path_in(
            "--log-file",
            std::path::Path::new("~\\x.log"),
            &home,
            cwd_at(&cwd),
        )
        .expect("literal value resolves");

        // Then it is an ordinary relative path whose first component is the
        // literal file name `~\x.log`.
        let expected = cwd.join("~\\x.log").expect("cwd joins a literal name");
        assert_eq!(resolved, expected);
    }

    #[rstest::rstest]
    #[test]
    fn absolute_value_is_verbatim_without_reading_the_cwd() {
        // Given an absolute value with user-typed `.` components and a cwd
        // supplier that always fails.
        let dir = jinn_testutil::tempdir();
        let home = home_at(&dir, "home");
        let raw = dir.path.join("./x.log").expect("temp root joins a name");

        // When resolving it.
        let resolved =
            resolve_cli_path_in("--log-file", raw.as_path(), &home, no_cwd).expect("absolute");

        // Then it round-trips verbatim — no normalization, no cwd read.
        assert_eq!(resolved, raw);
    }

    #[rstest::rstest]
    #[test]
    fn unavailable_cwd_error_names_the_flag_and_the_value() {
        // Given a relative value and a cwd supplier that always fails.
        let dir = jinn_testutil::tempdir();
        let home = home_at(&dir, "home");

        // When resolving `foo.log`.
        let result =
            resolve_cli_path_in("--log-file", std::path::Path::new("foo.log"), &home, no_cwd);

        // Then the failure renders the flag and the raw value.
        let report = result.expect_err("a relative value cannot resolve without a cwd");
        let rendered = format!("{report:?}");
        assert!(rendered.contains("--log-file"), "missing flag: {rendered}");
        assert!(rendered.contains("foo.log"), "missing value: {rendered}");
    }

    /// A typed absolute directory under the temp root, standing in for a
    /// home or cwd. `name` must be a bare filename.
    fn home_at(dir: &jinn_testutil::temp::JinnTempDir, name: &str) -> MarkedPathBuf<Absolute> {
        dir.path
            .join(name)
            .expect("temp root joins a bare filename")
    }
}
