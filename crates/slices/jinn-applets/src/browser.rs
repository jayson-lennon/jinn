//! The browser launch — the URL builder and the per-platform opener.
//!
//! The picker's `ENTER` resolves the highlighted app's URL and spawns
//! the OS's default-browser opener. The URL builder is pure and unit
//! tested; the launcher resolves a command by platform (`xdg-open` on
//! Linux, `open` on macOS, `start` via `cmd /c` on Windows) and is
//! unit tested on the *command resolution*, not on any browser actually
//! opening — a test that launches a browser is not a test.

/// Builds the URL an open lands on: the resolved bind as the base plus
/// the applet's path, always with the trailing slash so relative sibling
/// references in the artifact resolve.
#[must_use]
pub fn applet_url(resolved_bind: std::net::SocketAddr, id: &str) -> String {
    format!("http://{resolved_bind}/applets/{id}/")
}

/// Why a browser could not be launched.
#[derive(Debug, wherror::Error)]
#[error(debug)]
pub struct BrowserLaunchError;

/// Resolves the platform's opener argv for `url`.
///
/// `None` when the platform's opener is not installed (`xdg-open`
/// missing on Linux) — the caller surfaces a transient chat note rather
/// than pretending the browser opened.
#[must_use]
pub fn opener_command(url: &str) -> Option<Vec<String>> {
    if cfg!(target_os = "linux") {
        let path = which::which("xdg-open").ok()?;
        Some(vec![path.to_string_lossy().into_owned(), url.to_owned()])
    } else if cfg!(target_os = "macos") {
        Some(vec!["open".to_owned(), url.to_owned()])
    } else if cfg!(target_os = "windows") {
        Some(vec![
            "cmd".to_owned(),
            "/c".to_owned(),
            "start".to_owned(),
            String::new(), // the window-title placeholder `start` requires
            url.to_owned(),
        ])
    } else {
        None
    }
}

/// Spawns the opener for `url`, detached — the browser outlives jinn.
///
/// # Errors
/// Launches the user's default browser at `url`.
///
/// # Errors
///
/// Returns [`BrowserLaunchError`] when the platform has no opener or
/// the spawn itself fails. The URL is still correct; the user is told
/// the browser could not be opened.
pub fn open_in_browser(url: &str) -> Result<(), error_stack::Report<BrowserLaunchError>> {
    let Some(command) = opener_command(url) else {
        return Err(error_stack::Report::new(BrowserLaunchError)
            .attach(format!("no browser opener on this platform for {url}")));
    };
    let Some((program, args)) = command.split_first() else {
        // Unreachable by construction: every branch builds a non-empty
        // argv. Handled anyway so the function is total.
        return Err(error_stack::Report::new(BrowserLaunchError)
            .attach(format!("the opener command was empty for {url}")));
    };
    let spawned = std::process::Command::new(program).args(args).spawn();
    match spawned {
        Ok(_child) => Ok(()),
        Err(error) => Err(error_stack::Report::new(BrowserLaunchError)
            .attach(error.to_string())
            .attach(format!("failed to launch the browser for {url}"))),
    }
}

/// The transient note an open publishes when the bind is not up or the
/// launch fails — user-facing prose, not a code.
#[must_use]
pub fn open_failed_note(reason: &str) -> String {
    format!("could not open the applet in a browser: {reason}")
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::unwrap_used,
        reason = "test assertions need expect/unwrap"
    )]

    use super::{applet_url, open_failed_note, opener_command};

    #[rstest::rstest]
    #[test]
    fn url_carries_the_resolved_bind_and_trailing_slash() {
        // Given a resolved bind (port 0 already resolved by the server
        // actor) and an applet id.
        let bind: std::net::SocketAddr = "127.0.0.1:41234".parse().expect("bind parses");

        // When building the URL.
        let url = applet_url(bind, "sales-dashboard");

        // Then the URL names the applet path with the trailing slash, so
        // relative sibling references resolve.
        assert_eq!(url, "http://127.0.0.1:41234/applets/sales-dashboard/");
    }

    #[rstest::rstest]
    #[test]
    fn opener_resolves_on_this_platform() {
        // Given the current platform.
        // When resolving the opener.
        let command = opener_command("http://127.0.0.1:1/applets/a/");

        // Then some opener resolves on every supported platform — the
        // resolution is compile-time platform dispatch, and CI runs on
        // all three.
        assert!(command.is_some(), "no opener resolved on this platform");
        let argv = command.expect("checked above");
        assert_eq!(
            argv.last().expect("argv ends with the url"),
            "http://127.0.0.1:1/applets/a/"
        );
    }

    #[rstest::rstest]
    #[test]
    fn the_failure_note_is_user_facing_prose() {
        // Given a refusal reason.
        // When rendering the note.
        let note = open_failed_note("no browser opener on this platform");
        // Then it names the cause without a code.
        assert!(note.starts_with("could not open the applet in a browser:"));
        assert!(note.contains("no browser opener"));
    }

    #[rstest::rstest]
    #[test]
    fn app_url_is_built_from_a_catalog_entry_id() {
        // Given a catalog entry.
        let entry = jinn_applets_msg::AppletEntry {
            id: "quiz".to_owned(),
            title: "Quiz".to_owned(),
            created_at: "1".to_owned(),
        };
        let bind: std::net::SocketAddr = "[::1]:8123".parse().expect("bind parses");

        // When building the URL from its id.
        let url = applet_url(bind, entry.id.as_str());

        // Then the id is the path segment, unmodified.
        assert_eq!(url, "http://[::1]:8123/applets/quiz/");
    }
}
