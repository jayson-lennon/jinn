//! The shared daemon client — how every consumer reaches the browser
//! daemon.
//!
//! One discovery path for the bundled MCP server, the luau family, and
//! the `jinn browser` subcommands: read + prune the registry; a live
//! entry is the daemon; none means auto-start — spawn the daemon child
//! detached (it outlives jinn; that is the lifecycle contract), hold the
//! start lock while doing it so two concurrent auto-starters cannot
//! double-spawn, and poll the registry for the child's registration.
//!
//! The calls are blocking HTTP over the loopback port (reqwest's
//! blocking client — the same one the luau `Caps` already carries), so
//! the client is usable from synchronous contexts; the MCP server wraps
//! it in its async handlers with `spawn_blocking` at its own layer.

use crate::backend::{Snapshot, TabInfo, WaitCondition};
use crate::registry::{self, DaemonEntry};
use error_stack::Report;
use marked_path::MarkedPathAccess as _;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::io::Write as _;
use std::time::Duration;

/// How long one HTTP call may take — bounded above by the daemon's
/// per-command timeout plus margin.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(15);

/// How long the auto-start path waits for the child to register.
pub use crate::daemon::STARTUP_POLL_BUDGET;

/// Everything a client call needs.
#[derive(Clone)]
pub struct DaemonClient {
    http: reqwest::blocking::Client,
}

/// A failed daemon call or auto-start.
#[derive(Debug, wherror::Error)]
#[error(debug)]
pub enum ClientError {
    /// The daemon could not be reached or started.
    #[error("browser daemon unavailable: {0}")]
    Unavailable(String),
    /// The daemon answered with an envelope error.
    #[error("{0}")]
    Tool(String),
    /// The response could not be decoded.
    #[error("daemon response undecodable: {0}")]
    Decode(String),
}

impl DaemonClient {
    /// Builds a client with the shared call timeout.
    #[must_use]
    pub fn new() -> Self {
        Self {
            http: reqwest::blocking::Client::builder()
                .timeout(CALL_TIMEOUT)
                .build()
                .unwrap_or_default(),
        }
    }

    /// Resolves the live daemon, auto-starting one when the registry
    /// shows none.
    ///
    /// # Errors
    ///
    /// [`ClientError::Unavailable`] when the registry is unreadable, the
    /// spawn fails, or the child never registers within the budget.
    pub fn ensure_daemon(&self) -> Result<DaemonEntry, error_stack::Report<ClientError>> {
        use error_stack::ResultExt as _;
        let paths = jinn_common::app_paths::AppPaths::default();
        let registry_path = paths.browser_daemons_path();
        let mut live = registry::read_live(registry_path.as_path());
        if let Some(entry) = live.pop() {
            return Ok(entry);
        }
        self.auto_start(registry_path.as_path())
            .change_context(ClientError::Unavailable("auto-start failed".to_owned()))
    }

    /// Spawns the daemon child detached and polls the registry for its
    /// registration. Holding the start lock serializes concurrent
    /// auto-starters: the second starter finds the first's registration
    /// (or the fresh lock) instead of double-spawning.
    ///
    /// # Errors
    ///
    /// Names the failure: lock contention, spawn failure, or the
    /// registration budget expiring (the caller is pointed at the daemon
    /// log).
    pub fn auto_start(
        &self,
        registry_path: &std::path::Path,
    ) -> Result<DaemonEntry, Report<ClientError>> {
        use error_stack::Report;
        let lock_path = registry_path.with_file_name("browser-daemon.lock");
        // Holding the lock across spawn + registration-wait is the
        // race half of the two-daemon guard. A stale lock (a crashed
        // starter) is taken over.
        let _lock = crate::daemon::StartLock::acquire(lock_path).map_err(|e| {
            Report::new(ClientError::Unavailable(format!(
                "another process is starting the browser daemon — {e:?}"
            )))
        })?;

        // Re-check under the lock: the winner may have registered
        // between the caller's read and the lock acquisition.
        let mut live = registry::read_live(registry_path);
        if let Some(entry) = live.pop() {
            return Ok(entry);
        }

        let spawned = spawn_daemon_detached(std::process::id()).map_err(|e| {
            Report::new(ClientError::Unavailable(format!(
                "could not spawn `jinn browser daemon`: {e}"
            )))
        })?;
        drop(spawned);

        crate::daemon::wait_for_registration(registry_path, STARTUP_POLL_BUDGET).ok_or_else(|| {
            Report::new(ClientError::Unavailable(format!(
                "the browser daemon did not come up within {}s — check {}",
                STARTUP_POLL_BUDGET.as_secs(),
                jinn_common::app_paths::AppPaths::default()
                    .browser_daemon_log_path()
                    .as_path()
                    .display()
            )))
        })
    }

    /// One daemon call: JSON POST, envelope decode.
    fn call<T: DeserializeOwned>(
        &self,
        entry: &DaemonEntry,
        method: reqwest::Method,
        path: &str,
        body: Option<&impl Serialize>,
    ) -> Result<T, Report<ClientError>> {
        use error_stack::{Report, ResultExt as _};
        let url = format!("http://127.0.0.1:{}{path}", entry.port());
        let mut request = self.http.request(method, &url);
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request
            .send()
            .change_context(ClientError::Unavailable(format!(
                "the daemon at port {} stopped answering",
                entry.port()
            )))?;
        let payload: Envelope<T> = response
            .json()
            .change_context(ClientError::Decode("unreadable envelope".to_owned()))?;
        match payload {
            Envelope::Ok { data } => Ok(data),
            Envelope::Err { error } => Err(Report::new(ClientError::Tool(error))),
        }
    }

    // ── The eleven-tool surface ─────────────────────────────────────

    /// Opens a tab (optionally navigating).
    ///
    /// # Errors
    /// Tool errors carry the daemon's message.
    pub fn open_tab(&self, url: Option<&str>) -> Result<TabInfo, error_stack::Report<ClientError>> {
        #[derive(Serialize)]
        struct Body {
            url: Option<String>,
        }
        let entry = self.ensure_daemon()?;
        self.call(
            &entry,
            reqwest::Method::POST,
            "/tabs",
            Some(&Body {
                url: url.map(str::to_owned),
            }),
        )
    }

    /// Lists tabs.
    ///
    /// # Errors
    /// Tool errors carry the daemon's message.
    pub fn list_tabs(&self) -> Result<Vec<TabInfo>, error_stack::Report<ClientError>> {
        let entry = self.ensure_daemon()?;
        self.call(&entry, reqwest::Method::GET, "/tabs", Option::<&()>::None)
    }

    /// Navigates a tab.
    ///
    /// # Errors
    /// Tool errors carry the daemon's message.
    pub fn navigate(
        &self,
        tab: &str,
        url: &str,
    ) -> Result<TabInfo, error_stack::Report<ClientError>> {
        #[derive(Serialize)]
        struct Body {
            url: String,
        }
        let entry = self.ensure_daemon()?;
        self.call(
            &entry,
            reqwest::Method::POST,
            &format!("/tabs/{tab}/navigate"),
            Some(&Body {
                url: url.to_owned(),
            }),
        )
    }

    /// Snapshots a tab.
    ///
    /// # Errors
    /// Tool errors carry the daemon's message.
    pub fn snapshot(&self, tab: &str) -> Result<Snapshot, error_stack::Report<ClientError>> {
        let entry = self.ensure_daemon()?;
        self.call(
            &entry,
            reqwest::Method::POST,
            &format!("/tabs/{tab}/snapshot"),
            Option::<&()>::None,
        )
    }

    /// Clicks a ref.
    ///
    /// # Errors
    /// Tool errors carry the daemon's message.
    pub fn click(
        &self,
        tab: &str,
        reference: &str,
    ) -> Result<(), error_stack::Report<ClientError>> {
        #[derive(Serialize)]
        struct Body {
            reference: String,
        }
        let entry = self.ensure_daemon()?;
        self.call::<serde_json::Value>(
            &entry,
            reqwest::Method::POST,
            &format!("/tabs/{tab}/click"),
            Some(&Body {
                reference: reference.to_owned(),
            }),
        )
        .map(|_| ())
    }

    /// Clicks at viewport CSS-pixel coordinates (the daemon clamps).
    ///
    /// # Errors
    /// Tool errors carry the daemon's message.
    pub fn click_at(
        &self,
        tab: &str,
        x: f64,
        y: f64,
    ) -> Result<(), error_stack::Report<ClientError>> {
        #[derive(Serialize)]
        struct Body {
            x: f64,
            y: f64,
        }
        let entry = self.ensure_daemon()?;
        self.call::<serde_json::Value>(
            &entry,
            reqwest::Method::POST,
            &format!("/tabs/{tab}/click_at"),
            Some(&Body { x, y }),
        )
        .map(|_| ())
    }

    /// Types into a ref.
    ///
    /// # Errors
    /// Tool errors carry the daemon's message.
    pub fn type_text(
        &self,
        tab: &str,
        reference: &str,
        text: &str,
    ) -> Result<(), error_stack::Report<ClientError>> {
        #[derive(Serialize)]
        struct Body {
            reference: String,
            text: String,
        }
        let entry = self.ensure_daemon()?;
        self.call::<serde_json::Value>(
            &entry,
            reqwest::Method::POST,
            &format!("/tabs/{tab}/type"),
            Some(&Body {
                reference: reference.to_owned(),
                text: text.to_owned(),
            }),
        )
        .map(|_| ())
    }

    /// Presses a key.
    ///
    /// # Errors
    /// Tool errors carry the daemon's message.
    pub fn press(&self, tab: &str, key: &str) -> Result<(), error_stack::Report<ClientError>> {
        #[derive(Serialize)]
        struct Body {
            key: String,
        }
        let entry = self.ensure_daemon()?;
        self.call::<serde_json::Value>(
            &entry,
            reqwest::Method::POST,
            &format!("/tabs/{tab}/press"),
            Some(&Body {
                key: key.to_owned(),
            }),
        )
        .map(|_| ())
    }

    /// Evaluates a script through the stealth path.
    ///
    /// # Errors
    /// Tool errors carry the daemon's message.
    pub fn evaluate(
        &self,
        tab: &str,
        script: &str,
    ) -> Result<Option<serde_json::Value>, error_stack::Report<ClientError>> {
        #[derive(Serialize)]
        struct Body {
            script: String,
        }
        let entry = self.ensure_daemon()?;
        self.call(
            &entry,
            reqwest::Method::POST,
            &format!("/tabs/{tab}/evaluate"),
            Some(&Body {
                script: script.to_owned(),
            }),
        )
    }

    /// Screenshots a tab; the daemon saves a PNG and returns its path.
    ///
    /// # Errors
    /// Tool errors carry the daemon's message.
    pub fn screenshot(
        &self,
        tab: &str,
    ) -> Result<std::path::PathBuf, error_stack::Report<ClientError>> {
        #[derive(Deserialize)]
        struct Body {
            path: String,
        }
        let entry = self.ensure_daemon()?;
        let body: Body = self.call(
            &entry,
            reqwest::Method::POST,
            &format!("/tabs/{tab}/screenshot"),
            Option::<&()>::None,
        )?;
        Ok(std::path::PathBuf::from(body.path))
    }

    /// Waits for a condition.
    ///
    /// # Errors
    /// Tool errors carry the daemon's message.
    pub fn wait(
        &self,
        tab: &str,
        condition: &WaitCondition,
    ) -> Result<(), error_stack::Report<ClientError>> {
        #[derive(Serialize)]
        struct Body {
            condition: WaitCondition,
        }
        let entry = self.ensure_daemon()?;
        self.call::<serde_json::Value>(
            &entry,
            reqwest::Method::POST,
            &format!("/tabs/{tab}/wait"),
            Some(&Body {
                condition: condition.clone(),
            }),
        )
        .map(|_| ())
    }

    /// Closes a tab.
    ///
    /// # Errors
    /// Tool errors carry the daemon's message.
    pub fn close_tab(&self, tab: &str) -> Result<(), error_stack::Report<ClientError>> {
        let entry = self.ensure_daemon()?;
        self.call::<serde_json::Value>(
            &entry,
            reqwest::Method::DELETE,
            &format!("/tabs/{tab}"),
            Option::<&()>::None,
        )
        .map(|_| ())
    }

    /// `GET /health` — the daemon's own view (used by `jinn browser
    /// status`). Unlike the tool calls this one never auto-starts.
    ///
    /// # Errors
    ///
    /// [`ClientError::Unavailable`] when no daemon is live or the live
    /// entry stopped answering.
    pub fn health(&self) -> Result<HealthInfo, error_stack::Report<ClientError>> {
        use error_stack::{Report, ResultExt as _};
        let paths = jinn_common::app_paths::AppPaths::default();
        let registry_path = paths.browser_daemons_path();
        let mut live = registry::read_live(registry_path.as_path());
        let entry = live.pop().ok_or_else(|| {
            Report::new(ClientError::Unavailable(
                "no browser daemon is running".to_owned(),
            ))
        })?;
        let url = format!("http://127.0.0.1:{}/health", entry.port());
        let response = self
            .http
            .get(&url)
            .send()
            .change_context(ClientError::Unavailable(format!(
                "the daemon on port {} stopped answering",
                entry.port()
            )))?;
        let payload: Envelope<HealthInfo> = response
            .json()
            .change_context(ClientError::Decode("unreadable health envelope".to_owned()))?;
        match payload {
            Envelope::Ok { data } => Ok(data),
            Envelope::Err { error } => Err(Report::new(ClientError::Tool(error))),
        }
    }

    /// `POST /shutdown` — asks the daemon to exit (used by `jinn browser
    /// stop`). Never auto-starts.
    ///
    /// # Errors
    ///
    /// [`ClientError::Unavailable`] when no daemon is live or it stopped
    /// answering before the shutdown completed.
    pub fn shutdown(&self) -> Result<(), error_stack::Report<ClientError>> {
        use error_stack::{Report, ResultExt as _};
        let paths = jinn_common::app_paths::AppPaths::default();
        let registry_path = paths.browser_daemons_path();
        let mut live = registry::read_live(registry_path.as_path());
        let entry = live.pop().ok_or_else(|| {
            Report::new(ClientError::Unavailable(
                "no browser daemon is running".to_owned(),
            ))
        })?;
        let url = format!(
            "http://127.0.0.1:{}{}",
            entry.port(),
            crate::api::SHUTDOWN_PATH
        );
        self.http
            .post(&url)
            .send()
            .change_context(ClientError::Unavailable(format!(
                "the daemon on port {} stopped answering",
                entry.port()
            )))?;
        Ok(())
    }
}

impl Default for DaemonClient {
    fn default() -> Self {
        Self::new()
    }
}

/// `GET /health` data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthInfo {
    /// `live` or `dead`.
    pub browser: String,
    /// The resolved browser executable.
    pub binary: String,
    /// `config` or `auto-detected`.
    pub binary_source: String,
    /// Daemon uptime in milliseconds.
    pub uptime_ms: u64,
}

/// The one response envelope, client side.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Envelope<T> {
    Ok { data: T },
    Err { error: String },
}

/// Spawns `current_exe` with `browser daemon`, fully detached: null
/// stdio on every platform, a new session on unix (so the daemon
/// outlives the terminal and its process group), and the detached
/// creation flags on Windows.
fn spawn_daemon_detached(starter_pid: u32) -> std::io::Result<std::process::Child> {
    let exe = std::env::current_exe()?;
    let mut command = std::process::Command::new(exe);
    command.args(["browser", "daemon"]);
    // The daemon child takes over the fresh lock this spawner holds by
    // recognizing the recorded pid (parent on Unix; this env var is the
    // portable fallback where getppid is unavailable).
    command.env("JINN_BROWSER_STARTER_PID", starter_pid.to_string());
    command.stdin(std::process::Stdio::null());
    command.stdout(std::process::Stdio::null());
    // Chrome's diagnostics and the daemon's startup lines belong in the
    // daemon log, not on whichever terminal happened to trigger the
    // auto-start; append keeps restarts readable.
    let log_path = jinn_common::app_paths::AppPaths::default().browser_daemon_log_path();
    if let Some(parent) = log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    command.stderr(std::process::Stdio::from(log));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    let child = command.spawn()?;
    // Write a startup marker so a silent failure is diagnosable from the
    // log alone.
    let _ = writeln!(
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)?,
        "[{}] spawned daemon child pid={}",
        now_millis(),
        child.id()
    );
    Ok(child)
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
