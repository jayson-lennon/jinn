//! The shared browser-binary detector.
//!
//! One mechanism, many candidate lists: a [`BinaryProbe`] walks PATH
//! names first (via `which`) and platform install paths second (typed
//! absolute as built), and the caller supplies the ordered names its
//! feature prefers. The kiosk launcher keeps its Chromium-family list
//! (Edge and Brave included); the browser daemon keeps its
//! branded-Chrome-first list (no Edge/Brave — the stealth tuning targets
//! Chrome). The mechanism is the shared part; the preference is not.
//!
//! Resolved binaries are typed [`MarkedPathBuf`]`<Absolute>` on arrival:
//! PATH-resolved and install-path candidates are absolute by
//! construction, and command builders need the owned form.

use marked_path::{Absolute, MarkedPathBuf};

/// Finds a browser binary: a PATH lookup or an install-path check.
///
/// The resolution seam — tests inject a probe that reports candidates
/// without touching the filesystem or `$PATH`. Production rides `which`
/// and `Path::exists`.
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

/// The first resolved candidate: `path_candidates` in order, then the
/// platform install paths. PATH names always precede install paths —
/// the user-directed precedence.
#[must_use]
pub fn resolve_browser_binary(
    probe: &dyn BinaryProbe,
    path_candidates: &[&str],
) -> Option<MarkedPathBuf<Absolute>> {
    path_candidates
        .iter()
        .find_map(|name| probe.resolve_name(name))
        .or_else(|| resolve_from_install_paths(probe))
}

/// Windows install-path candidates as `{env}\<suffix>` pairs, tried in
/// order. The environment variable is read only inside this installer,
/// per the environment-variable rule (startup-time resolution in the
/// daemon's case).
#[cfg(target_os = "windows")]
const WINDOWS_ENV_CANDIDATES: &[(&str, &str)] = &[
    ("PROGRAMFILES", r"Google\Chrome\Application\chrome.exe"),
    ("PROGRAMFILES(X86)", r"Google\Chrome\Application\chrome.exe"),
    ("PROGRAMFILES", r"Chromium\Application\chromium.exe"),
    ("LOCALAPPDATA", r"Chromium\Application\chromium.exe"),
];

/// Windows install paths that do not depend on an environment variable.
#[cfg(target_os = "windows")]
const WINDOWS_ABSOLUTE_CANDIDATES: &[&str] = &[
    r"C:\Program Files\Google\Chrome\Application\chrome.exe",
    r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
];

/// macOS `.app` bundle executables, first hit wins.
#[cfg(target_os = "macos")]
const MACOS_INSTALL_PATHS: &[&str] = &[
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Chromium.app/Contents/MacOS/Chromium",
];

/// The install-path stage of [`resolve_browser_binary`]: every candidate
/// is typed `MarkedPathBuf<Absolute>` as it is built; an env-less or
/// non-absolute candidate is skipped, never owned raw. Existence is
/// asked of the injected probe, never the real filesystem — the seam
/// holds on every platform.
#[cfg(target_os = "windows")]
fn resolve_from_install_paths(probe: &dyn BinaryProbe) -> Option<MarkedPathBuf<Absolute>> {
    use marked_path::MarkedPathAccess as _;
    let candidates = WINDOWS_ENV_CANDIDATES
        .iter()
        .filter_map(|(env_var, suffix)| {
            std::env::var(env_var)
                .ok()
                .map(|base| format!(r"{base}\{suffix}"))
        })
        .chain(
            WINDOWS_ABSOLUTE_CANDIDATES
                .iter()
                .map(|path| (*path).to_owned()),
        );
    candidates
        .filter_map(|candidate| MarkedPathBuf::<Absolute>::new(candidate).ok())
        .find(|path| probe.path_exists(path.as_path()))
}

/// The install-path stage of [`resolve_browser_binary`].
#[cfg(target_os = "macos")]
fn resolve_from_install_paths(probe: &dyn BinaryProbe) -> Option<MarkedPathBuf<Absolute>> {
    use marked_path::MarkedPathAccess as _;
    MACOS_INSTALL_PATHS
        .iter()
        .filter_map(|candidate| MarkedPathBuf::<Absolute>::new(*candidate).ok())
        .find(|path| probe.path_exists(path.as_path()))
}

/// The install-path stage of [`resolve_browser_binary`]: none on Linux
/// and other targets — distro packages land on PATH.
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn resolve_from_install_paths(_probe: &dyn BinaryProbe) -> Option<MarkedPathBuf<Absolute>> {
    None
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        reason = "test assertions need expect to state what went wrong"
    )]

    use super::{BinaryProbe, MarkedPathBuf, resolve_browser_binary};
    use marked_path::{Absolute, MarkedPathAccess as _};
    use std::sync::Arc;
    use std::sync::Mutex;

    /// The fake PATH hit the recording probe answers with, shaped for
    /// the target platform: `MarkedPathBuf<Absolute>` rejects a
    /// POSIX-only path on Windows, where absolute means a drive prefix.
    fn fake_binary(name: &str) -> String {
        if cfg!(target_os = "windows") {
            format!(r"C:\bin\{name}.exe")
        } else {
            format!("/usr/bin/{name}")
        }
    }

    /// A probe recording every consulted candidate, answering only for
    /// its scripted name — the filesystem is never touched.
    struct RecordingProbe {
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
                return MarkedPathBuf::<Absolute>::new(fake_binary(name)).ok();
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

    /// The daemon's candidate list — branded Chrome before Chromium,
    /// no Edge/Brave. Mirrors `DAEMON_PATH_CANDIDATES` in the daemon's
    /// binary resolution; kept here so drift between the two is a test
    /// failure.
    const DAEMON_CANDIDATES: &[&str] = &[
        "google-chrome",
        "google-chrome-stable",
        "chrome",
        "chromium",
        "chromium-browser",
    ];

    #[rstest::rstest]
    #[test]
    fn candidates_are_consulted_in_priority_order() {
        // Given a probe that resolves nothing.
        let probe = RecordingProbe::resolving_nothing();

        // When resolving against the daemon list.
        let _ = resolve_browser_binary(probe.as_ref(), DAEMON_CANDIDATES);

        // Then every candidate was consulted, in order.
        let consulted = probe.consulted.lock().expect("probe lock");
        assert_eq!(consulted.as_slice(), DAEMON_CANDIDATES);
    }

    #[rstest::rstest]
    #[test]
    fn the_first_resolved_candidate_wins() {
        // Given a probe answering only for the third candidate.
        let probe = RecordingProbe::answering_for("chrome");

        // When resolving.
        let binary = resolve_browser_binary(probe.as_ref(), DAEMON_CANDIDATES);

        // Then the first resolved candidate wins and the probe stopped
        // asking.
        let binary = binary.expect("a candidate resolved");
        assert_eq!(binary.to_string_lossy(), fake_binary("chrome"));
        assert_eq!(
            probe.consulted.lock().expect("probe lock").as_slice(),
            ["google-chrome", "google-chrome-stable", "chrome"]
        );
    }

    #[rstest::rstest]
    #[test]
    fn resolution_returns_none_when_every_candidate_misses() {
        // Given a probe resolving nothing.
        let probe = RecordingProbe::resolving_nothing();

        // When resolving.
        let binary = resolve_browser_binary(probe.as_ref(), DAEMON_CANDIDATES);

        // Then nothing resolves — the caller surfaces the error.
        assert!(binary.is_none());
    }
}
