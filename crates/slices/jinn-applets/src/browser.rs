//! The browser launch — the URL builder and the per-platform opener.
//!
//! The picker's `ENTER` resolves the highlighted app's URL and spawns
//! the OS's default-browser opener. The URL builder is pure and unit
//! tested; the launcher resolves a command by platform (`xdg-open` on
//! Linux, `open` on macOS, `start` via `cmd /c` on Windows) and is
//! unit tested on the *command resolution*, not on any browser actually
//! opening — a test that launches a browser is not a test.
//!
//! The picker's `<c-enter>` kiosk open rides the same doctrine through
//! [`KioskLauncher`]: the built-in path resolves a Chromium-family
//! binary through an injectable [`BinaryProbe`] (PATH names first,
//! platform install paths second) and spawns it with jinn-owned flags
//! against jinn's own profile; a configured `[applets] kiosk_command`
//! template replaces all of it and runs through the platform shell with
//! verbatim placeholder substitution. Resolution, substitution, and
//! argv assembly are pure and unit tested; the spawn itself never runs
//! in a test.

use error_stack::Report;
use marked_path::MarkedPathAccess as _;
use marked_path::{Absolute, MarkedPathBuf};

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

/// Builds a `Command` whose stdio is detached from jinn's terminal.
///
/// A browser jinn spawned with inherited stdio paints its own stderr —
/// Wayland/Vulkan/GLib diagnostics, crash noise — straight over the
/// TUI, since both processes share the same pty. Nulling all three
/// streams silences the child at the descriptor level: nothing to
/// redirect on the user's side, and no output is lost anywhere it
/// mattered (a browser's stderr is diagnostics, not data).
fn detached_command(program: &str) -> std::process::Command {
    let mut command = std::process::Command::new(program);
    command.stdin(std::process::Stdio::null());
    command.stdout(std::process::Stdio::null());
    command.stderr(std::process::Stdio::null());
    command
}

/// Spawns the OS opener for `url`, detached — the browser outlives jinn.
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
    let spawned = detached_command(program).args(args).spawn();
    match spawned {
        Ok(_child) => Ok(()),
        Err(error) => Err(error_stack::Report::new(BrowserLaunchError)
            .attach(error.to_string())
            .attach(format!("failed to launch the browser for {url}"))),
    }
}

/// The prose a failed open reports into the chat — user-facing, not a
/// code.
///
/// `reason` is the launcher report's debug chain: the type name plus
/// every attachment (the looked-for families, the exit code), which is
/// the part the user needs.
#[must_use]
pub fn open_failed_note(reason: &str) -> String {
    format!("could not open the applet in a browser: {reason}")
}

// ── Kiosk launch ────────────────────────────────────────────────────────

/// The query marker a kiosk open appends to the applet URL.
///
/// One string, appended in one builder, so a page can adapt to kiosk
/// (its own styling keys on it) while a normal `ENTER` open keeps the
/// plain URL — the two builders share [`applet_url`], so the bases can
/// never disagree.
pub const KIOSK_QUERY_MARKER: &str = "?kiosk=1";

/// Builds the URL a kiosk open lands on: the applet URL plus the
/// `?kiosk=1` marker, so the page can adapt its chrome for kiosk mode.
#[must_use]
pub fn applet_kiosk_url(resolved_bind: std::net::SocketAddr, id: &str) -> String {
    format!("{}{KIOSK_QUERY_MARKER}", applet_url(resolved_bind, id))
}

/// Everything the kiosk launcher needs for one launch.
#[derive(Debug, Clone)]
pub struct KioskLaunchRequest {
    /// The kiosk URL — [`applet_kiosk_url`]'s output, marker included.
    pub url: String,
    /// The jinn-owned kiosk profile directory (`AppPaths::kiosk_profile_dir`).
    pub profile: MarkedPathBuf<Absolute>,
    /// The applet's directory id.
    pub id: String,
    /// The applet's display title (falls back to the id when the catalog
    /// lacks the entry).
    pub title: String,
}

/// Why a kiosk window could not be launched.
#[derive(Debug, wherror::Error)]
#[error(debug)]
pub struct KioskLaunchError;

/// Finds a browser binary: a PATH lookup or an install-path check.
///
/// The resolution seam for the built-in launcher — tests inject a probe
/// that reports candidates without touching the filesystem or `$PATH`.
/// Production rides `which` and `Path::exists`. Resolved binaries are
/// typed [`MarkedPathBuf`]`<Absolute>` on arrival: PATH-resolved and
/// install-path candidates are absolute by construction, and the argv
/// builder needs the owned form.
pub trait BinaryProbe: Send + Sync {
    /// Resolves a bare executable name against `$PATH`.
    fn resolve_name(&self, name: &str) -> Option<MarkedPathBuf<Absolute>>;
    /// Whether an install path exists on disk.
    fn path_exists(&self, path: &std::path::Path) -> bool;
    /// Returns the name of this backend for debugging.
    fn name(&self) -> &'static str;
}

/// The production probe: `which` for PATH names, the filesystem for
/// install paths.
#[derive(Debug, Clone)]
pub struct WhichProbe;

impl BinaryProbe for WhichProbe {
    fn resolve_name(&self, name: &str) -> Option<MarkedPathBuf<Absolute>> {
        let path = which::which(name).ok()?;
        // A PATH hit is always absolute (one PATH dir joined with the
        // name); a non-absolute result would be a broken PATH.
        MarkedPathBuf::<Absolute>::new(path).ok()
    }

    fn path_exists(&self, path: &std::path::Path) -> bool {
        path.exists()
    }

    fn name(&self) -> &'static str {
        "which"
    }
}

/// PATH executable names tried in order — Chrome, Chromium, Edge, Brave.
///
/// Order is the launch preference; the user-directed precedent puts PATH
/// names before platform install paths. Linux ships no install-path
/// stage: distro packages land on PATH.
const PATH_CANDIDATES: &[&str] = &[
    "google-chrome",
    "google-chrome-stable",
    "chrome",
    "chromium",
    "chromium-browser",
    "microsoft-edge",
    "microsoft-edge-stable",
    "brave",
    "brave-browser",
];

/// The first resolved candidate, PATH names before install paths.
///
/// The install-path stage is platform-conditional: macOS `.app` bundle
/// executables and Windows `Program Files` / `%LOCALAPPDATA%` layouts
/// (including `msedge.exe`, which ships on every Windows box). Linux has
/// no stage — distro packages are on PATH.
#[must_use]
pub fn resolve_kiosk_binary(probe: &dyn BinaryProbe) -> Option<MarkedPathBuf<Absolute>> {
    PATH_CANDIDATES
        .iter()
        .find_map(|name| probe.resolve_name(name))
        .or_else(resolve_kiosk_binary_from_install_paths)
}

/// Windows install-path candidates as `{env}\<suffix>` pairs, tried in
/// order. The environment variable is read only inside the installer
/// below, per the environment-variable rule.
#[cfg(target_os = "windows")]
const WINDOWS_ENV_CANDIDATES: &[(&str, &str)] = &[
    ("PROGRAMFILES", r"Google\Chrome\Application\chrome.exe"),
    ("PROGRAMFILES(X86)", r"Google\Chrome\Application\chrome.exe"),
    (
        "PROGRAMFILES(X86)",
        r"Microsoft\Edge\Application\msedge.exe",
    ),
    ("LOCALAPPDATA", r"Google\Chrome\Application\chrome.exe"),
];

/// Windows install paths that do not depend on an environment variable —
/// Edge ships on every Windows box.
#[cfg(target_os = "windows")]
const WINDOWS_EDGE_FALLBACKS: &[&str] = &[
    r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
    r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
];

/// macOS `.app` bundle executables, first hit wins.
#[cfg(target_os = "macos")]
const MACOS_INSTALL_PATHS: &[&str] = &[
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Chromium.app/Contents/MacOS/Chromium",
    "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
    "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
];

/// The install-path stage of [`resolve_kiosk_binary`]: every candidate
/// is typed `MarkedPathBuf<Absolute>` as it is built; an env-less or
/// non-absolute candidate is skipped, never owned raw.
#[cfg(target_os = "windows")]
fn resolve_kiosk_binary_from_install_paths() -> Option<MarkedPathBuf<Absolute>> {
    let candidates = WINDOWS_ENV_CANDIDATES
        .iter()
        .filter_map(|(env_var, suffix)| {
            std::env::var(env_var)
                .ok()
                .map(|base| format!(r"{base}\{suffix}"))
        })
        .chain(WINDOWS_EDGE_FALLBACKS.iter().map(|path| (*path).to_owned()));
    candidates
        .filter_map(|candidate| MarkedPathBuf::<Absolute>::new(candidate).ok())
        .find(|path| WhichProbe.path_exists(path.as_path()))
}

/// The install-path stage of [`resolve_kiosk_binary`].
#[cfg(target_os = "macos")]
fn resolve_kiosk_binary_from_install_paths() -> Option<MarkedPathBuf<Absolute>> {
    MACOS_INSTALL_PATHS
        .iter()
        .filter_map(|candidate| MarkedPathBuf::<Absolute>::new(*candidate).ok())
        .find(|path| WhichProbe.path_exists(path.as_path()))
}

/// The install-path stage of [`resolve_kiosk_binary`]: none on Linux and
/// other targets — distro packages land on PATH.
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn resolve_kiosk_binary_from_install_paths() -> Option<MarkedPathBuf<Absolute>> {
    None
}

/// The jinn-owned Chromium flags the built-in kiosk launch passes.
///
/// A closed list: `--kiosk` engages the mode; `--app` makes the window
/// the applet instead of a browser window; `--user-data-dir` is the
/// jinn profile without which Chromium silently ignores `--kiosk` when
/// the user's browser is already running; the rest suppress first-run
/// chrome and mobile-style gestures, and let `<audio>`/`<video>` start
/// without a user gesture.
const KIOSK_FLAGS: &[&str] = &[
    "--kiosk",
    "--no-first-run",
    "--no-default-browser-check",
    "--disable-pinch",
    "--overscroll-history-navigation=0",
    "--autoplay-policy=no-user-gesture-required",
];

/// Assembles the built-in kiosk argv: the resolved binary, the fixed
/// jinn-owned flag set, `--app=<url>`, and `--user-data-dir=<profile>`.
///
/// Flag order is irrelevant to Chromium; the two value-carrying flags
/// trail so the fixed list reads as one block.
#[must_use]
pub fn builtin_kiosk_argv(
    binary: &MarkedPathBuf<Absolute>,
    request: &KioskLaunchRequest,
) -> Vec<String> {
    let mut argv: Vec<String> = std::iter::once(binary.to_string_lossy().into_owned())
        .chain(KIOSK_FLAGS.iter().map(|flag| (*flag).to_owned()))
        .collect();
    argv.push(format!("--app={}", request.url));
    argv.push(format!(
        "--user-data-dir={}",
        request.profile.to_string_lossy()
    ));
    argv
}

/// Substitutes the four template tokens verbatim: `{url}`, `{profile}`,
/// `{id}`, `{title}`.
///
/// Verbatim means verbatim — jinn adds no quoting of any kind; quoting
/// is the template author's (the shipped example in `default_jinn.toml`
/// carries the correct shape). Values appear anywhere the token does,
/// including inside user-written quotes.
#[must_use]
pub fn substitute_kiosk_template(template: &str, request: &KioskLaunchRequest) -> String {
    template
        .replace("{url}", &request.url)
        .replace("{profile}", &request.profile.to_string_lossy())
        .replace("{id}", &request.id)
        .replace("{title}", &request.title)
}

/// The substituted template wrapped for the platform shell: `sh -c` on
/// unix, `cmd /C` on Windows.
#[must_use]
pub fn shell_command(substituted: &str) -> Vec<String> {
    if cfg!(target_os = "windows") {
        vec!["cmd".to_owned(), "/C".to_owned(), substituted.to_owned()]
    } else {
        vec!["sh".to_owned(), "-c".to_owned(), substituted.to_owned()]
    }
}

/// Spawns `argv` detached — the browser outlives jinn; no wait, no
/// cleanup. The same detach shape the OS opener uses.
///
/// # Errors
///
/// Returns [`KioskLaunchError`] when the spawn itself fails.
fn spawn_detached(argv: &[String]) -> Result<KioskChild, Report<KioskLaunchError>> {
    let Some((program, launch_args)) = argv.split_first() else {
        // Unreachable by construction: both builders produce non-empty
        // argv. Handled anyway so the function is total.
        return Err(Report::new(KioskLaunchError).attach("the kiosk launch command was empty"));
    };
    match detached_command(program).args(launch_args).spawn() {
        Ok(child) => Ok(KioskChild::new(child)),
        Err(error) => Err(Report::new(KioskLaunchError)
            .attach(error.to_string())
            .attach(format!("failed to spawn {program:?}"))),
    }
}

/// Resolves the kiosk argv — everything before the spawn, pure.
///
/// A configured `command_override` **replaces the built-in launcher
/// entirely**: the template is substituted and wrapped for the platform
/// shell, and binary resolution never runs. `None` resolves a
/// Chromium-family binary (PATH names first, install paths second).
///
/// # Errors
///
/// Returns [`KioskLaunchError`] when no binary resolves — naming the
/// families tried and the config escape hatch.
fn kiosk_argv(
    request: &KioskLaunchRequest,
    command_override: Option<&str>,
    probe: &dyn BinaryProbe,
) -> Result<Vec<String>, Report<KioskLaunchError>> {
    match command_override {
        Some(template) => {
            let substituted = substitute_kiosk_template(template, request);
            Ok(shell_command(&substituted))
        }
        None => {
            let Some(binary) = resolve_kiosk_binary(probe) else {
                return Err(Report::new(KioskLaunchError).attach(
                    "no Chromium-family browser found (looked for google-chrome, \
                     chromium, microsoft-edge, brave on PATH, then their install \
                     paths) — set [applets] kiosk_command to launch another browser",
                ));
            };
            Ok(builtin_kiosk_argv(&binary, request))
        }
    }
}

/// Launches one kiosk window, detached — the browser outlives jinn.
///
/// # Errors
///
/// Returns [`KioskLaunchError`] when no binary resolves or the spawn
/// itself fails. The caller surfaces the error; this never panics.
pub fn launch_kiosk(
    request: &KioskLaunchRequest,
    command_override: Option<&str>,
    probe: &dyn BinaryProbe,
) -> Result<KioskChild, Report<KioskLaunchError>> {
    let argv = kiosk_argv(request, command_override, probe)?;
    spawn_detached(&argv)
}

/// A kiosk process jinn successfully spawned.
///
/// The launcher cannot block on `wait()` — a browser lives for hours —
/// so it hands the child back and the caller decides how to observe it:
/// [`KioskChild::report_if_failed_fast`] turns an instant nonzero exit
/// (a mistyped `kiosk_command`, a missing binary behind the shell) into
/// the failure text, while a browser that starts and later closes is a
/// normal kiosk session and reports nothing.
pub struct KioskChild {
    /// The detached child, held so its exit status can be observed.
    child: std::process::Child,
}

impl KioskChild {
    /// Wraps a spawned child.
    #[must_use]
    pub fn new(child: std::process::Child) -> Self {
        Self { child }
    }

    /// Takes the child back, for a caller that must reap a process the
    /// grace watch deliberately left running.
    #[must_use]
    pub fn into_child(self) -> std::process::Child {
        self.child
    }

    /// Waits up to `grace` for the child to exit; reports only a
    /// nonzero exit inside the window.
    ///
    /// A command that cannot run (`sh -c "bogus"`, exit 127) fails
    /// instantly; a browser that actually started keeps running past
    /// the grace window and this answers `Ok` without waiting for
    /// it. The child is reaped when it exits.
    ///
    /// # Errors
    ///
    /// Returns [`KioskLaunchError`] when the child exited nonzero
    /// within the grace window or the wait itself failed.
    pub fn report_if_failed_fast(
        mut self,
        grace: std::time::Duration,
    ) -> Result<(), Report<KioskLaunchError>> {
        let deadline = std::time::Instant::now() + grace;
        let status = loop {
            match self.child.try_wait() {
                // Still running past the grace window: a healthy kiosk
                // session — leave it running and report nothing.
                Ok(None) if std::time::Instant::now() >= deadline => return Ok(()),
                Ok(None) => std::thread::sleep(POLL_INTERVAL),
                Ok(Some(status)) => break status,
                Err(error) => {
                    return Err(Report::new(KioskLaunchError)
                        .attach(error.to_string())
                        .attach("observing the kiosk process failed"));
                }
            }
        };
        match status.code() {
            Some(0) => Ok(()),
            Some(code) => Err(Report::new(KioskLaunchError)
                .attach(format!("the kiosk process exited with code {code}"))),
            None => Err(Report::new(KioskLaunchError)
                .attach("the kiosk process was terminated by a signal")),
        }
    }
}

/// How often a fast-fail watch polls a child's exit status.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(25);

/// The kiosk launcher trait: one method, one error.
///
/// The seam that lets the store actor's kiosk handler be tested without
/// spawning anything: production wraps [`launch_kiosk`] with the real
/// probe; tests inject a recording or failing backend. Lives beside the
/// launch machinery it abstracts.
pub trait KioskLauncher: Send + Sync {
    /// Launches the kiosk window for `request`, honoring the configured
    /// override (which replaces the built-in launcher entirely).
    ///
    /// Returns the spawned child so the caller can observe an instant
    /// nonzero exit — the failure mode a spawn cannot catch, since a
    /// shell wrapping a bad command starts fine and dies immediately.
    ///
    /// # Errors
    ///
    /// Returns [`KioskLaunchError`] when the launcher could not run —
    /// no binary resolved or the spawn failed.
    fn launch(
        &self,
        request: &KioskLaunchRequest,
        command_override: Option<&str>,
    ) -> Result<KioskChild, Report<KioskLaunchError>>;

    /// Returns the name of this backend for debugging.
    fn name(&self) -> &'static str;
}

/// The production launcher: [`launch_kiosk`] with the real probe.
#[derive(Debug, Clone)]
pub struct RealKioskLauncher;

impl KioskLauncher for RealKioskLauncher {
    fn launch(
        &self,
        request: &KioskLaunchRequest,
        command_override: Option<&str>,
    ) -> Result<KioskChild, Report<KioskLaunchError>> {
        launch_kiosk(request, command_override, &WhichProbe)
    }

    fn name(&self) -> &'static str {
        "real"
    }
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
    #[cfg(target_os = "linux")]
    #[test]
    fn a_detached_spawn_never_inherits_the_terminal_streams() {
        // Given a child spawned through the detach helper — `sleep`,
        // alive long enough to inspect.
        let mut child = super::detached_command("sleep")
            .arg("5")
            .spawn()
            .expect("sleep spawns");

        // When reading the child's stdio fd links from procfs.
        let pid = child.id();
        for fd in ["0", "1", "2"] {
            let link = format!("/proc/{pid}/fd/{fd}");
            let target = std::fs::read_link(std::path::Path::new(&link))
                .expect("the child's fd link exists");

            // Then every stream points at the null device — never at
            // the pty a browser would paint its diagnostics over.
            assert_eq!(
                target,
                std::path::Path::new("/dev/null"),
                "fd {fd} of a detached child"
            );
        }

        // Reap the child.
        let _ = child.kill();
        let _ = child.wait();
    }

    #[rstest::rstest]
    #[cfg(target_family = "unix")]
    #[test]
    fn an_instant_nonzero_exit_reports_as_a_fast_failure() {
        // Given a child that cannot run — `sh -c` on a command the
        // shell does not know, the mistyped-`kiosk_command` shape.
        let child = super::detached_command("sh")
            .args(["-c", "definitely-not-a-real-command"])
            .spawn()
            .expect("the shell spawns");

        // When watching it through the grace window.
        let result =
            super::KioskChild::new(child).report_if_failed_fast(std::time::Duration::from_secs(5));

        // Then the failure is reported, naming the exit code.
        let report = result.expect_err("a bogus command fails");
        let text = format!("{report:?}");
        assert!(text.contains("code"), "got: {text}");
    }

    #[rstest::rstest]
    #[cfg(target_family = "unix")]
    #[test]
    fn a_running_child_outlives_the_grace_window_and_reports_nothing() {
        // Given a child that stays up — the started-browser shape.
        let spawned = super::detached_command("sleep")
            .arg("30")
            .spawn()
            .expect("sleep spawns");
        let pid = spawned.id();

        // When watching it through a short grace window.
        let result = super::KioskChild::new(spawned)
            .report_if_failed_fast(std::time::Duration::from_millis(150));

        // Then nothing is reported — a browser that started is a
        // healthy kiosk session, however long it later runs.
        assert!(result.is_ok(), "got: {result:?}");

        // Reap the child the watch deliberately left running: the
        // watch consumed the handle, so cleanup goes by pid.
        let _ = std::process::Command::new("kill")
            .args([pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }

    #[rstest::rstest]
    #[cfg(target_family = "unix")]
    #[test]
    fn a_zero_exit_inside_the_grace_window_reports_nothing() {
        // Given a child that exits successfully right away.
        let child = super::detached_command("true")
            .spawn()
            .expect("true spawns");

        // When watching it through the grace window.
        let result =
            super::KioskChild::new(child).report_if_failed_fast(std::time::Duration::from_secs(5));

        // Then nothing is reported — a clean exit is not a failure.
        assert!(result.is_ok());
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

    // ── Kiosk launch tests ─────────────────────────────────────────────
    //
    // Resolution, substitution, and argv assembly are asserted at the
    // command-resolution level with an injected probe — a test that
    // launches a browser is not a test.

    use super::applet_kiosk_url;
    use super::builtin_kiosk_argv;
    use super::shell_command;
    use super::substitute_kiosk_template;
    use super::{BinaryProbe, KioskLaunchRequest, resolve_kiosk_binary};
    use marked_path::{Absolute, MarkedPathAccess as _, MarkedPathBuf};
    use std::sync::Arc;
    use std::sync::Mutex;

    /// A probe recording every consulted candidate, answering only for
    /// its scripted name — the filesystem is never touched.
    struct RecordingProbe {
        /// The PATH name this probe answers for; every other name
        /// misses, so a test can pin which candidate won.
        answers_for: &'static str,
        consulted: Mutex<Vec<String>>,
    }

    impl RecordingProbe {
        fn answering_for(name: &'static str) -> Arc<Self> {
            Arc::new(Self {
                answers_for: name,
                consulted: Mutex::new(Vec::new()),
            })
        }

        fn resolving_nothing() -> Arc<Self> {
            Self::answering_for("")
        }
    }

    impl BinaryProbe for RecordingProbe {
        fn resolve_name(&self, name: &str) -> Option<MarkedPathBuf<Absolute>> {
            self.consulted
                .lock()
                .expect("probe lock")
                .push(name.to_owned());
            if name == self.answers_for {
                return MarkedPathBuf::<Absolute>::new(format!("/usr/bin/{name}")).ok();
            }
            None
        }

        fn path_exists(&self, _path: &std::path::Path) -> bool {
            false
        }

        fn name(&self) -> &'static str {
            "recording"
        }
    }

    /// One kiosk launch request against a temp profile dir.
    fn launch_request() -> KioskLaunchRequest {
        let bind: std::net::SocketAddr = "127.0.0.1:41234".parse().expect("bind parses");
        let profile = jinn_testutil::tempdir()
            .path
            .join("kiosk-profile")
            .expect("join");
        KioskLaunchRequest {
            url: applet_kiosk_url(bind, "quiz"),
            profile,
            id: "quiz".to_owned(),
            title: "The Quiz".to_owned(),
        }
    }

    #[rstest::rstest]
    #[test]
    fn kiosk_url_appends_the_kiosk_marker_to_the_plain_url() {
        // Given a resolved bind and an applet id.
        let bind: std::net::SocketAddr = "127.0.0.1:41234".parse().expect("bind parses");

        // When building both URLs.
        let plain = applet_url(bind, "quiz");
        let kiosk = applet_kiosk_url(bind, "quiz");

        // Then the kiosk URL is exactly the plain URL plus the marker —
        // the bases can never disagree, and a normal open stays plain.
        assert_eq!(kiosk, format!("{plain}?kiosk=1"));
        assert!(!plain.contains("kiosk"));
    }

    #[rstest::rstest]
    #[test]
    fn candidates_order_path_names_before_platform_paths() {
        // Given a probe that resolves nothing — resolution must fall
        // through every PATH name in order.
        let probe = RecordingProbe::resolving_nothing();

        // When resolving the binary.
        let _ = resolve_kiosk_binary(probe.as_ref());

        // Then the probe saw the PATH names in their priority order
        // (Chrome first, Brave last), all before any install path could
        // be consulted.
        let consulted = probe.consulted.lock().expect("probe lock");
        assert_eq!(
            consulted.first().map(String::as_str),
            Some("google-chrome"),
            "the first PATH candidate is google-chrome; got {consulted:?}"
        );
        assert_eq!(
            consulted.last().map(String::as_str),
            Some("brave-browser"),
            "the last PATH candidate is brave-browser; got {consulted:?}"
        );
        assert_eq!(consulted.len(), 9, "all nine PATH names are consulted");
    }

    #[rstest::rstest]
    #[test]
    fn the_first_resolved_path_name_wins() {
        // Given a probe answering only for `chromium` — the third
        // candidate.
        let probe = RecordingProbe::answering_for("chromium");

        // When resolving the binary.
        let binary = resolve_kiosk_binary(probe.as_ref());

        // Then the first resolved candidate wins and the probe stopped
        // asking — PATH-first preference holds.
        let binary = binary.expect("a candidate resolved");
        assert!(binary.starts_with("/usr/bin/chromium"));
        let consulted = probe.consulted.lock().expect("probe lock");
        assert_eq!(
            consulted.as_slice(),
            [
                "google-chrome",
                "google-chrome-stable",
                "chrome",
                "chromium"
            ]
        );
    }

    #[rstest::rstest]
    #[test]
    fn resolution_returns_none_when_every_candidate_misses() {
        // Given a probe resolving nothing.
        let probe = RecordingProbe::resolving_nothing();

        // When resolving the binary.
        let binary = resolve_kiosk_binary(probe.as_ref());

        // Then nothing resolves — the caller surfaces the error.
        assert!(binary.is_none());
    }

    #[rstest::rstest]
    #[test]
    fn kiosk_argv_carries_the_jinn_owned_flags_and_profile() {
        // Given a resolved binary and a launch request.
        let binary = MarkedPathBuf::<Absolute>::new("/usr/bin/chromium").expect("absolute");
        let request = launch_request();

        // When assembling the built-in argv.
        let argv = builtin_kiosk_argv(&binary, &request);

        // Then the binary leads, followed by the jinn-owned flags: the
        // kiosk mode, the applet URL, and jinn's own profile dir (the
        // flag that keeps kiosk alive beside a running browser).
        assert_eq!(argv.first().map(String::as_str), Some("/usr/bin/chromium"));
        assert!(argv.contains(&"--kiosk".to_owned()));
        assert!(argv.contains(&format!("--app={}", request.url)));
        assert!(
            argv.iter()
                .any(|arg| arg.starts_with("--user-data-dir=") && arg.ends_with("kiosk-profile"))
        );
        // And the remaining flags are all present.
        for flag in [
            "--no-first-run",
            "--no-default-browser-check",
            "--disable-pinch",
            "--overscroll-history-navigation=0",
            "--autoplay-policy=no-user-gesture-required",
        ] {
            assert!(argv.contains(&flag.to_owned()), "missing {flag}");
        }
    }

    #[rstest::rstest]
    #[test]
    fn template_substitutes_all_four_tokens_verbatim() {
        // Given a template quoting two of the four values itself.
        let template =
            r#"chromium --kiosk --app="{url}" --user-data-dir="{profile}" --x={id} --y={title}"#;
        let request = launch_request();

        // When substituting.
        let substituted = substitute_kiosk_template(template, &request);

        // Then every token is replaced with its verbatim value — jinn
        // added no quotes of its own; the author's quotes remain.
        assert!(
            substituted.contains(&format!("--app=\"{}\"", request.url)),
            "got: {substituted}"
        );
        assert!(
            substituted.contains(&format!(
                "--user-data-dir=\"{}\"",
                request.profile.as_path().to_string_lossy()
            )),
            "got: {substituted}"
        );
        assert!(substituted.contains("--x=quiz"), "got: {substituted}");
        assert!(substituted.contains("--y=The Quiz"), "got: {substituted}");
    }

    #[rstest::rstest]
    #[test]
    fn template_run_selects_the_platform_shell() {
        // Given a substituted command.
        let substituted = "chromium --kiosk --app=x";

        // When wrapping it for the platform shell.
        let argv = shell_command(substituted);

        // Then the shell is `sh -c` on unix, `cmd /C` on Windows, with
        // the command as one verbatim argument.
        #[cfg(target_os = "windows")]
        assert_eq!(argv, ["cmd", "/C", substituted]);
        #[cfg(not(target_os = "windows"))]
        assert_eq!(argv, ["sh", "-c", substituted]);
    }

    #[rstest::rstest]
    #[test]
    fn a_template_override_skips_binary_resolution_entirely() {
        // Given a probe that would record any consultation.
        let probe = RecordingProbe::answering_for("chromium");
        let request = launch_request();

        // When resolving the argv with the override set.
        let argv = super::kiosk_argv(
            &request,
            Some("chromium --kiosk --app={url}"),
            probe.as_ref(),
        )
        .expect("the override resolves without any binary lookup");

        // Then the probe was never consulted — the template replaced
        // the built-in launcher entirely — and the argv is the shell
        // wrapping the substituted template.
        assert!(
            probe.consulted.lock().expect("probe lock").is_empty(),
            "binary resolution must not run when a template is set"
        );
        let substituted = format!("chromium --kiosk --app={}", request.url);
        #[cfg(target_os = "windows")]
        assert_eq!(argv, ["cmd", "/C", substituted.as_str()]);
        #[cfg(not(target_os = "windows"))]
        assert_eq!(argv, ["sh", "-c", substituted.as_str()]);
    }

    #[rstest::rstest]
    #[test]
    fn a_builtin_launch_without_a_browser_names_the_cause() {
        // Given a probe resolving nothing and no template.
        let probe = RecordingProbe::resolving_nothing();
        let request = launch_request();

        // When resolving the argv.
        let result = super::kiosk_argv(&request, None, probe.as_ref());

        // Then the error names the families tried and the config
        // escape hatch — the user-facing sentence, not a code.
        let error = result.expect_err("no browser resolves");
        let text = format!("{error:?}");
        assert!(text.contains("kiosk_command"), "got: {text}");
        assert!(text.contains("Chromium"), "got: {text}");
    }
}
