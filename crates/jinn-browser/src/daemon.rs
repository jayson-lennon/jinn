//! The browser daemon process: startup resolution, two-daemon guard,
//! supervision, and the serve loop.
//!
//! This module runs only in the `jinn browser daemon` child process. It
//! is entrypoint infrastructure, not composition: it never opens the
//! session store, never starts actors, and reads the config document
//! scoped to `[browser]` — its one deliberate divergence from the
//! bundled-MCP children — because it is the process that resolves the
//! browser binary. A config edit applies at the next daemon start
//! (`jinn browser restart`); there is no live-reload path.
//!
//! Startup, in order:
//! 1. Live-entry refusal — the registry's reader-pruned view already
//!    shows a live daemon, so this process exits nonzero naming it.
//!    (Self-healing half of the two-daemon guard.)
//! 2. Binary resolution — `[browser].binary` when configured (absolute
//!    and must exist: an unresolvable configured path refuses with a
//!    visible error — the `[shell]` refusal pattern scoped to the
//!    daemon), else the shared detector's branded-Chrome-first walk.
//! 3. Start lock — `<state>/browser-daemon.lock`, exclusive create;
//!    concurrent auto-starters serialize here, the loser re-checks the
//!    registry and exits quietly if the winner registered (race half
//!    of the two-daemon guard).
//! 4. Launch — headed Chrome through the chaser launcher, persistent
//!    profile dir, then bind the loopback API and register.

use crate::backend::{ChaserBackend, ChaserLauncher};
use crate::registry::{self, DaemonEntry};
use error_stack::Report;
use marked_path::MarkedPathAccess as _;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How long a start lock may sit untouched before it is stale and may
/// be taken over.
const LOCK_STALENESS: Duration = Duration::from_secs(30);

/// How long a spawner may take between spawning the daemon child and the
/// child registering itself.
pub const STARTUP_POLL_BUDGET: Duration = Duration::from_secs(30);

/// How often the auto-start path polls the registry for the child's
/// entry.
const STARTUP_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// How often the supervision tick runs.
const SUPERVISION_INTERVAL: Duration = Duration::from_secs(1);

/// The daemon's resolved launch configuration: everything the serve loop
/// needs that was decided before Chrome came up.
#[derive(Debug, Clone)]
pub struct ResolvedLaunch {
    /// The browser executable that was resolved.
    pub executable: std::path::PathBuf,
    /// Where the decision came from (surfaced by `/health`).
    pub source: BinarySource,
}

/// Where the resolved browser binary came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinarySource {
    /// The `[browser] binary` key in jinn.toml.
    Config,
    /// Auto-detected (PATH names, then install paths).
    AutoDetected,
}

impl BinarySource {
    /// The user-facing name for `/health` and `jinn browser status`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Config => "config",
            Self::AutoDetected => "auto-detected",
        }
    }
}

/// Why the daemon refused to start.
#[derive(Debug, wherror::Error)]
#[error(debug)]
pub enum DaemonRefusal {
    /// A daemon is already live per the registry.
    #[error("a browser daemon is already running (pid {pid}, port {port})")]
    AlreadyLive { pid: u32, port: u16 },
    /// The configured `[browser].binary` is not an absolute path.
    #[error("[browser] binary must be an absolute path: {path}")]
    NotAbsolute { path: String },
    /// The configured `[browser].binary` does not exist on disk.
    #[error("[browser] binary does not exist: {path}")]
    MissingBinary { path: String },
    /// No browser binary could be auto-detected.
    #[error(
        "no browser found (looked for google-chrome, google-chrome-stable, chrome, \
         chromium, chromium-browser on PATH, then their install paths) — set \
         [browser] binary in jinn.toml to an absolute path"
    )]
    NoBrowser,
    /// The daemon's own profile dir could not be created.
    #[error("cannot create the browser-daemon profile dir: {0}")]
    ProfileDir(String),
}

/// Reads the `[browser]` section from the config document at `path`,
/// returning just that section. A missing or unreadable document reads
/// as the default (auto-detect): the daemon is optional tooling and a
/// broken config file must not silently break `jinn`'s launch either
/// way — the launch-time validation reports config problems; the daemon
/// only needs this one section.
///
/// # Errors
///
/// Returns the parse error only when the file exists but is not valid
/// TOML at all — a state the launch-time validator would also refuse,
/// so the daemon reports it rather than guessing.
pub fn read_browser_section(
    path: &std::path::Path,
) -> Result<jinn_preferences_config::schemas::BrowserConfig, error_stack::Report<DaemonRefusal>> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Ok(jinn_preferences_config::schemas::BrowserConfig::default());
    };
    let doc = raw.parse::<toml::Table>().map_err(|e| {
        error_stack::Report::new(DaemonRefusal::ProfileDir(format!(
            "the jinn.toml at {} is not valid TOML: {e}",
            path.display()
        )))
    })?;
    Ok(doc
        .get("browser")
        .cloned()
        .and_then(|v| {
            <jinn_preferences_config::schemas::BrowserConfig as serde::Deserialize>::deserialize(v)
                .ok()
        })
        .unwrap_or_default())
}

/// Resolves the browser binary the daemon will launch.
///
/// A configured path must be absolute and must exist — the refusal
/// names the configured path, per the `[shell]` pattern. Unset falls to
/// the shared detector with the daemon's branded-Chrome-first list
/// (no Edge/Brave: the stealth tuning targets Chrome).
///
/// # Errors
///
/// [`DaemonRefusal::NotAbsolute`] or [`DaemonRefusal::MissingBinary`]
/// for a configured path that does not resolve;
/// [`DaemonRefusal::NoBrowser`] when nothing is detectable.
pub fn resolve_browser(
    config: &jinn_preferences_config::schemas::BrowserConfig,
    probe: &dyn jinn_common::binary_detect::BinaryProbe,
) -> Result<ResolvedLaunch, Report<DaemonRefusal>> {
    let configured = match config.resolved_binary() {
        Ok(Some(path)) => Some(path),
        Ok(None) => None,
        Err(e) => {
            let path = config.binary.clone().unwrap_or_default();
            return Err(Report::new(DaemonRefusal::NotAbsolute { path }).attach(format!("{e:?}")));
        }
    };
    if let Some(path) = configured {
        let p = std::path::Path::new(path);
        if !p.is_absolute() {
            return Err(Report::new(DaemonRefusal::NotAbsolute {
                path: path.to_owned(),
            }));
        }
        // Existence rides the injected probe like every other filesystem
        // question in the resolver, so tests stay hermetic.
        if !probe.path_exists(p) {
            return Err(Report::new(DaemonRefusal::MissingBinary {
                path: path.to_owned(),
            }));
        }
        return Ok(ResolvedLaunch {
            executable: p.to_path_buf(),
            source: BinarySource::Config,
        });
    }
    let candidates: &[&str] = &[
        "google-chrome",
        "google-chrome-stable",
        "chrome",
        "chromium",
        "chromium-browser",
    ];
    let detected = jinn_common::binary_detect::resolve_browser_binary(probe, candidates)
        .ok_or(Report::new(DaemonRefusal::NoBrowser))?;
    Ok(ResolvedLaunch {
        executable: detected.as_path().to_path_buf(),
        source: BinarySource::AutoDetected,
    })
}

/// Refuses when the registry already shows a live daemon.
///
/// # Errors
///
/// [`DaemonRefusal::AlreadyLive`] names the live entry's pid and port.
pub fn refuse_if_live(
    paths: &jinn_common::app_paths::AppPaths,
) -> Result<(), Report<DaemonRefusal>> {
    let registry_path = paths.browser_daemons_path();
    let live = registry::read_live(registry_path.as_path());
    if let Some(existing) = live.first() {
        return Err(Report::new(DaemonRefusal::AlreadyLive {
            pid: existing.pid(),
            port: existing.port(),
        }));
    }
    Ok(())
}

/// The start lock: exclusive-create file carrying pid + timestamp.
///
/// Held from just before the Chrome launch to just after registration.
/// A lock older than [`LOCK_STALENESS`] (or carrying a pid that matches
/// no live registry entry) is taken over — a crashed daemon must not
/// wedge every future start.
pub struct StartLock {
    path: std::path::PathBuf,
}

/// Why the start lock could not be acquired.
#[derive(Debug, wherror::Error)]
#[error(debug)]
pub enum LockError {
    /// Another starter holds the lock and it is not stale.
    #[error("another process is starting the browser daemon (lock at {path})")]
    Held { path: String },
    /// The lock file could not be read, created, or written.
    #[error("start lock unavailable: {0}")]
    Io(String),
}

impl StartLock {
    /// Acquires the lock at `path`, taking over a stale one.
    ///
    /// A fresh lock whose recorded pid is this process's **parent** is
    /// taken over: the auto-start spawner holds the lock across spawn +
    /// registration-wait, so the daemon child it spawned would otherwise
    /// see its own spawner's fresh lock and refuse to start — every
    /// auto-start killing itself. Any other fresh holder is refused.
    ///
    /// # Errors
    ///
    /// [`LockError::Held`] when a fresh, unrelated lock exists;
    /// [`LockError::Io`] when the filesystem objects.
    pub fn acquire(path: std::path::PathBuf) -> Result<Self, Report<LockError>> {
        use error_stack::Report;
        use std::io::Write as _;
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let my_pid = std::process::id();
        // The auto-start spawner is this process's parent (client.rs
        // spawns `current_exe browser daemon`). A fresh lock held by
        // that pid is the spawner's own serialization lock, not a
        // competing starter.
        #[cfg(unix)]
        let parent_pid = u32::from(std::os::unix::process::parent_id());
        #[cfg(not(unix))]
        let parent_pid = u32::try_from(
            std::env::var("JINN_BROWSER_STARTER_PID")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0),
        )
        .unwrap_or(0);
        if let Some(holder) = read_lock(path.as_path()) {
            let stale_by_age = holder.age > LOCK_STALENESS;
            // Parent-takeover: the auto-start spawner (this child's
            // parent) holds the serialization lock across the spawn;
            // without the takeover the child refuses and every
            // auto-start kills itself. A lock recording this own pid is
            // NOT a takeover case — a second acquire in the same
            // process must still be refused (that is the double-start
            // guard within one process, and the daemon never re-acquires
            // its own lock).
            let parent_holds = holder.pid == parent_pid;
            if !(stale_by_age || parent_holds) {
                return Err(Report::new(LockError::Held {
                    path: path.to_string_lossy().into_owned(),
                }));
            }
            tracing::info!(
                holder = holder.pid,
                mine = my_pid,
                parent = parent_pid,
                "taking over the browser-daemon start lock (stale, self, or spawner)"
            );
        }
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .map_err(|e| Report::new(LockError::Io(e.to_string())))?;
        let _ = writeln!(file, "pid={}\nstarted={}", my_pid, now_millis());
        Ok(Self { path })
    }
}

/// A start lock's recorded holder.
struct LockHolder {
    pid: u32,
    age: std::time::Duration,
}

/// Reads a start lock's holder, if the file parses. An unreadable or
/// malformed lock reads as absent (the open() below then overwrites it).
fn read_lock(path: &std::path::Path) -> Option<LockHolder> {
    let raw = std::fs::read_to_string(path).ok()?;
    let mut pid = None;
    let mut started = None;
    for line in raw.lines() {
        if let Some(v) = line.strip_prefix("pid=") {
            pid = v.parse().ok();
        } else if let Some(v) = line.strip_prefix("started=") {
            started = v.parse().ok();
        }
    }
    let (pid, started) = (pid?, started?);
    let age = now_millis()
        .checked_sub(started)
        .map_or(std::time::Duration::ZERO, std::time::Duration::from_millis);
    Some(LockHolder { pid, age })
}

impl Drop for StartLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Epoch milliseconds.
fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// The daemon's own serve loop. Runs until [`SHUTDOWN_PATH`] is hit or
/// the process is killed; on the way out the registry entry is removed
/// and the browser child is shut down.
///
/// # Errors
///
/// Startup refusals (already-live, unresolvable binary, lock contention,
/// profile dir) come back as [`DaemonRefusal`] reports. A serve-phase
/// failure (the API bind failing, Chrome refusing to launch) is
/// reported the same way after cleanup.
pub async fn serve(
    paths: &jinn_common::app_paths::AppPaths,
    config: &jinn_preferences_config::schemas::BrowserConfig,
    probe: &dyn jinn_common::binary_detect::BinaryProbe,
) -> Result<(), Report<DaemonRefusal>> {
    use error_stack::Report;

    refuse_if_live(paths)?;
    let resolved = resolve_browser(config, probe)?;
    tracing::info!(
        binary = %resolved.executable.display(),
        source = resolved.source.as_str(),
        "browser binary resolved"
    );

    let profile_dir = paths.browser_daemon_profile_dir();
    std::fs::create_dir_all(profile_dir.as_path()).map_err(|e| {
        Report::new(DaemonRefusal::ProfileDir(e.to_string()))
            .attach(format!("path: {}", profile_dir.as_path().display()))
    })?;

    // The lock spans the launch so concurrent auto-starters serialize.
    let lock_path = paths
        .browser_daemons_path()
        .as_path()
        .with_file_name("browser-daemon.lock");
    let _lock = StartLock::acquire(lock_path).map_err(|e| {
        Report::new(DaemonRefusal::ProfileDir(format!("{e:?}")))
            .attach("while acquiring the browser-daemon start lock")
    })?;

    // Re-check the registry under the lock: the lock loser path exits
    // here instead of double-launching.
    refuse_if_live(paths)?;

    let backend = Arc::new(ChaserBackend::new(
        Arc::new(ChaserLauncher),
        resolved.executable.clone(),
        profile_dir.as_path().to_path_buf(),
    ));

    // The shutdown channel: the router's /shutdown endpoint flips the
    // sender; the serve await watches the receiver.
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    let app = crate::api::router(crate::api::DaemonState {
        backend: backend.clone(),
        executable: resolved.executable.clone(),
        binary_source: resolved.source.as_str().to_owned(),
        started_at: now_millis(),
        screenshots_dir: paths.browser_screenshots_dir().as_path().to_path_buf(),
        shutdown: shutdown_tx,
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| Report::new(DaemonRefusal::ProfileDir(format!("api bind failed: {e}"))))?;
    let port = listener
        .local_addr()
        .map_err(|e| Report::new(DaemonRefusal::ProfileDir(format!("api addr failed: {e}"))))?
        .port();

    let entry = DaemonEntry::new(std::process::id(), port, now_millis());
    let registry_path = paths.browser_daemons_path();
    if let Some(parent) = registry_path.as_path().parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    registry::register(registry_path.as_path(), entry.clone()).map_err(|e| {
        Report::new(DaemonRefusal::ProfileDir(format!(
            "registry write failed: {e}"
        )))
    })?;
    tracing::info!(
        pid = entry.pid(),
        port = entry.port(),
        "browser daemon serving"
    );

    // The registry path outlives the borrow of `paths`; clone it for the
    // cleanup task.
    let registry_for_cleanup = registry_path.as_path().to_path_buf();
    let own_pid = entry.pid();

    // The supervision tick: a death verdict marks the backend dead; the
    // next command recycles lazily.
    let supervisor = {
        let backend = backend.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(SUPERVISION_INTERVAL);
            loop {
                ticker.tick().await;
                if let Some(signal) = backend.poll_supervision().await {
                    tracing::debug!(?signal, "supervision tick observed a death");
                }
            }
        })
    };

    // Serve until /shutdown flips the watch.
    let serve_result = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let mut rx = shutdown_rx;
            let _ = rx.changed().await;
        })
        .await;

    supervisor.abort();
    let _ = backend.shutdown_browser().await;
    let _ = registry::remove_entry(&registry_for_cleanup, own_pid);

    serve_result
        .map_err(|e| Report::new(DaemonRefusal::ProfileDir(format!("serve failed: {e}"))))?;
    Ok(())
}

/// Polls the registry until a daemon registers or the budget runs out.
/// The auto-start path's wait half.
#[must_use]
pub fn wait_for_registration(
    registry_path: &std::path::Path,
    budget: Duration,
) -> Option<DaemonEntry> {
    let deadline = std::time::Instant::now() + budget;
    while std::time::Instant::now() < deadline {
        let mut live = registry::read_live(registry_path);
        if let Some(entry) = live.pop() {
            return Some(entry);
        }
        std::thread::sleep(STARTUP_POLL_INTERVAL);
    }
    None
}
