//! The browser control seam.
//!
//! [`BrowserBackend`] is the only way the daemon touches Chrome. Everything
//! above the seam (HTTP API, MCP tools, luau family) speaks daemon tab ids
//! and plain data; everything below it is chaser-oxide, and chaser types
//! never cross upward — not even as trait-object payloads: [`EngineHandle`]
//! and [`PageHandle`] are the abstract shapes the supervised backend
//! drives, and the production impls wrap chaser privately. That is what
//! makes the seam testable without a real Chrome.
//!
//! The one deliberate stealth property: the seam has *no* raw-JS-evaluation
//! entry — [`PageHandle::evaluate_stealth`] is the only eval, and even the
//! page title is read through it (`Page::get_title` rides the raw path that
//! enables `Runtime`, which the fork exists to avoid).

use std::collections::HashMap;
use std::sync::Arc;

use chaser_oxide::cdp::browser_protocol::target::TargetId;
use serde::Deserialize;
use tokio::sync::Mutex;

pub use crate::surface::{Snapshot, SnapshotRef, TabInfo, WaitCondition};

/// Errors the seam reports upward.
#[derive(Debug, wherror::Error)]
#[error(debug)]
pub enum BackendError {
    /// The browser process died; the next command recycles.
    #[error("browser is dead: {0}")]
    DeadBrowser(String),
    /// The tab id names no open tab (closed by hand, or cleared by recycle).
    #[error("unknown tab: {0}")]
    UnknownTab(String),
    /// The element ref is from an older snapshot; a fresh one is needed.
    #[error("stale ref {0}: take a fresh snapshot")]
    StaleRef(String),
    /// A command timed out. A timeout is a command failure, not a death
    /// verdict — only the supervision signals flip the browser to dead.
    #[error("command timed out")]
    Timeout,
    /// Anything the engine itself reported.
    #[error("browser command failed: {0}")]
    Engine(String),
}

/// The page-level operations the seam drives. The abstraction over one
/// open tab; the production impl wraps [`chaser_oxide::ChaserPage`].
///
/// Every evaluation here is the stealth path — there is no raw eval on
/// this trait, by construction.
#[async_trait::async_trait]
pub trait PageHandle: Send + Sync {
    /// Navigates the page.
    ///
    /// # Errors
    ///
    /// Engine failures map onto [`BackendError`].
    async fn goto(&self, url: &str) -> Result<(), BackendError>;

    /// The page's current URL.
    ///
    /// # Errors
    ///
    /// Engine failures map onto [`BackendError`].
    async fn url(&self) -> Result<Option<String>, BackendError>;

    /// The page's title, read through the stealth path (never
    /// `Page::get_title`, which enables `Runtime`).
    ///
    /// # Errors
    ///
    /// Engine failures map onto [`BackendError`].
    async fn title(&self) -> Result<Option<String>, BackendError>;

    /// Evaluates a script in the fork's isolated world. The only eval on
    /// the seam.
    ///
    /// # Errors
    ///
    /// Engine failures map onto [`BackendError`].
    async fn evaluate_stealth(
        &self,
        script: &str,
    ) -> Result<Option<serde_json::Value>, BackendError>;

    /// Moves the mouse along a human curve and clicks.
    ///
    /// # Errors
    ///
    /// Engine failures map onto [`BackendError`].
    async fn click_human(&self, x: f64, y: f64) -> Result<(), BackendError>;

    /// Human-types text (per-key press/release with pacing).
    ///
    /// # Errors
    ///
    /// Engine failures map onto [`BackendError`].
    async fn type_text(&self, text: &str) -> Result<(), BackendError>;

    /// Presses a key on the focused element.
    ///
    /// # Errors
    ///
    /// Engine failures map onto [`BackendError`].
    async fn press_key(&self, key: &str) -> Result<(), BackendError>;

    /// Captures a PNG screenshot.
    ///
    /// # Errors
    ///
    /// Engine failures map onto [`BackendError`].
    async fn screenshot(&self) -> Result<Vec<u8>, BackendError>;

    /// Closes the page.
    ///
    /// # Errors
    ///
    /// Engine failures map onto [`BackendError`].
    async fn close(&self) -> Result<(), BackendError>;
}

/// The browser-level operations the seam drives: tab creation and
/// resolution, child liveness, shutdown. The production impl wraps
/// [`chaser_oxide::Browser`] behind a mutex that is locked only for these
/// operations — page work runs on cloned page handles without it.
#[async_trait::async_trait]
pub trait EngineHandle: Send + Sync {
    /// Opens a new page and returns its chaser target id plus the page
    /// handle.
    ///
    /// # Errors
    ///
    /// Engine failures map onto [`BackendError`].
    async fn new_page(&self, url: &str) -> Result<(TargetId, Arc<dyn PageHandle>), BackendError>;

    /// Resolves (or re-attaches) the page handle for a target id.
    ///
    /// # Errors
    ///
    /// A target that no longer exists maps onto [`BackendError::Engine`].
    async fn get_page(&self, target: &TargetId) -> Result<Arc<dyn PageHandle>, BackendError>;

    /// Polls the browser child without blocking: `Some(status)` when the
    /// child has exited.
    ///
    /// # Errors
    ///
    /// An io failure reading the child state is reported, not swallowed.
    async fn try_wait(&self) -> std::io::Result<Option<String>>;

    /// Shuts the browser child down (best effort).
    ///
    /// # Errors
    ///
    /// Engine failures map onto [`BackendError`].
    async fn shutdown(&self) -> Result<(), BackendError>;
}

/// A live engine handed to the backend by a launcher: the abstract handle
/// plus the death channel. The driver task behind the handler stream
/// writes `Some(reason)` on a handler error or an unexpected stream end;
/// `None` means alive.
pub struct LiveEngine {
    /// The browser-level handle.
    pub handle: Box<dyn EngineHandle>,
    /// Death channel: `Some(reason)` once the handler stream has died.
    pub death: tokio::sync::watch::Receiver<Option<String>>,
}

/// The backend seam over tabs: the surface the HTTP API serves. Every
/// method carries the tab explicitly — the trait cannot express an action
/// on a "current" tab, which is the design's central exclusion.
#[async_trait::async_trait]
pub trait BrowserBackend: Send + Sync {
    /// Opens a new tab, optionally navigating immediately.
    ///
    /// # Errors
    ///
    /// Backend errors name the failure for tool-level rendering.
    async fn open_tab(&self, url: Option<&str>) -> Result<TabInfo, BackendError>;

    /// Lists every open tab.
    ///
    /// # Errors
    ///
    /// Backend errors name the failure for tool-level rendering.
    async fn list_tabs(&self) -> Result<Vec<TabInfo>, BackendError>;

    /// Navigates a tab.
    ///
    /// # Errors
    ///
    /// Backend errors name the failure for tool-level rendering.
    async fn navigate(&self, tab: &str, url: &str) -> Result<TabInfo, BackendError>;

    /// Snapshots a tab: interactive refs plus a readable tree.
    ///
    /// # Errors
    ///
    /// Backend errors name the failure for tool-level rendering.
    async fn snapshot(&self, tab: &str) -> Result<Snapshot, BackendError>;

    /// Human-clicks the element a ref names.
    ///
    /// # Errors
    ///
    /// Backend errors name the failure for tool-level rendering.
    async fn click(&self, tab: &str, reference: &str) -> Result<(), BackendError>;

    /// Human-types text into the element a ref names (click to focus,
    /// then per-key typing).
    ///
    /// # Errors
    ///
    /// Backend errors name the failure for tool-level rendering.
    async fn type_text(&self, tab: &str, reference: &str, text: &str) -> Result<(), BackendError>;

    /// Presses a key on the focused element (`Enter`, `Tab`, `ArrowDown`,
    /// … — the fork's key vocabulary).
    ///
    /// # Errors
    ///
    /// Backend errors name the failure for tool-level rendering.
    async fn press_key(&self, tab: &str, key: &str) -> Result<(), BackendError>;

    /// Evaluates JavaScript in the tab through the fork's isolated-world
    /// path only. Returns the JSON value the script produced.
    ///
    /// # Errors
    ///
    /// Backend errors name the failure for tool-level rendering.
    async fn evaluate(
        &self,
        tab: &str,
        script: &str,
    ) -> Result<Option<serde_json::Value>, BackendError>;

    /// Screenshots the tab and returns the PNG bytes.
    ///
    /// # Errors
    ///
    /// Backend errors name the failure for tool-level rendering.
    async fn screenshot(&self, tab: &str) -> Result<Vec<u8>, BackendError>;

    /// Waits for a condition on a tab.
    ///
    /// # Errors
    ///
    /// Backend errors name the failure for tool-level rendering.
    async fn wait(&self, tab: &str, condition: &WaitCondition) -> Result<(), BackendError>;

    /// Closes a tab.
    ///
    /// # Errors
    ///
    /// Backend errors name the failure for tool-level rendering.
    async fn close_tab(&self, tab: &str) -> Result<(), BackendError>;

    /// Whether a live browser engine is up (the `/health` view; a dead
    /// engine still answers commands — the next one recycles).
    async fn is_live(&self) -> bool;
}

/// Why the browser was pronounced dead.
#[derive(Debug, Clone)]
pub enum DeathSignal {
    /// The handler event stream errored or ended unexpectedly.
    HandlerStream(String),
    /// The child exited (`try_wait` returned an exit status).
    Exited(String),
}

/// Launches a connected engine. The seam the supervisor uses to recycle:
/// a fake counts launches in tests; production builds the chaser stack and
/// spawns the handler-driver task.
#[async_trait::async_trait]
pub trait EngineLauncher: Send + Sync {
    /// Launches the browser and returns the live engine.
    ///
    /// The implementation owns driving the handler event stream (the
    /// production launcher spawns a task for it) and reports its death
    /// through the returned channel.
    ///
    /// # Errors
    ///
    /// A launch failure names what went wrong.
    async fn launch(
        &self,
        executable: &std::path::Path,
        user_data_dir: &std::path::Path,
    ) -> Result<LiveEngine, BackendError>;
}

/// The supervised backend: the concrete [`BrowserBackend`] the daemon runs.
///
/// Owns the tab table and the death signals. A death signal — the handler
/// stream's channel flipping, the child exiting under the supervision
/// tick, or a command reporting a dead transport — flips the state to
/// dead; the *next* command recycles lazily: relaunching with the same
/// resolved config, clearing the tab table (every tab died with the old
/// Chrome), and running. Commands on tabs from before the recycle report
/// [`BackendError::UnknownTab`], which is the designed recovery — the
/// caller lists tabs and re-opens. Engine-level commands (`open_tab`,
/// `list_tabs`) recycle transparently.
pub struct ChaserBackend<L: EngineLauncher> {
    launcher: Arc<L>,
    executable: std::path::PathBuf,
    user_data_dir: std::path::PathBuf,
    inner: Mutex<BackendState>,
    /// Per-command timeout — a command failure, never a death verdict.
    command_timeout: std::time::Duration,
}

struct BackendState {
    /// `Some` while a browser is live.
    engine: Option<Engine>,
    /// Minted tab ids in mint order; the id → target mapping died with any
    /// previous browser and is empty after a recycle. The mint counter is
    /// *not* reset: ids are never reused.
    tabs: Vec<MintedTab>,
    /// The next tab id suffix.
    next_tab: u64,
    /// The ref tables, per tab id, latest snapshot wins.
    refs: HashMap<String, Vec<RefEntry>>,
}

struct Engine {
    handle: Box<dyn EngineHandle>,
    /// Death channel from the launcher; polled by supervision and between
    /// commands.
    death: tokio::sync::watch::Receiver<Option<String>>,
    /// Page handles resolved for this engine, keyed by chaser target id.
    pages: HashMap<TargetId, Arc<dyn PageHandle>>,
}

#[derive(Debug, Clone)]
struct MintedTab {
    /// The public id (`t3`).
    public: String,
    /// The chaser target id it maps to.
    target: TargetId,
}

/// One cached interactive element: the center coordinates the click path
/// needs. Display metadata lives in the snapshot response, not here.
struct RefEntry {
    id: String,
    center: (f64, f64),
}

/// How long a single backend command may run.
pub const COMMAND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

impl<L: EngineLauncher> ChaserBackend<L> {
    /// Builds the backend over a launcher with the resolved binary.
    #[must_use]
    pub fn new(
        launcher: Arc<L>,
        executable: std::path::PathBuf,
        user_data_dir: std::path::PathBuf,
    ) -> Self {
        Self {
            launcher,
            executable,
            user_data_dir,
            inner: Mutex::new(BackendState {
                engine: None,
                tabs: Vec::new(),
                next_tab: 1,
                refs: HashMap::new(),
            }),
            command_timeout: COMMAND_TIMEOUT,
        }
    }

    /// Overrides the per-command timeout. Tests shorten it; production
    /// keeps [`COMMAND_TIMEOUT`].
    #[must_use]
    pub fn with_command_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.command_timeout = timeout;
        self
    }

    /// The supervision tick, run by the daemon's periodic task. Checks the
    /// handler death channel first (the drive task's verdict), then the
    /// child exit. A verdict clears the engine state so the next command
    /// recycles.
    pub async fn poll_supervision(&self) -> Option<DeathSignal> {
        let mut state = self.inner.lock().await;
        let engine = state.engine.as_ref()?;
        // The channel flag is consumed by has_changed; clone the reason out
        // so the watch guard is gone before the state clears.
        let handler_reason = if engine.death.has_changed().unwrap_or(false) {
            engine.death.borrow().clone()
        } else {
            None
        };
        if let Some(reason) = handler_reason {
            tracing::warn!(%reason, "supervision: handler stream died");
            Self::clear_dead(&mut state);
            return Some(DeathSignal::HandlerStream(reason));
        }
        match engine.handle.try_wait().await {
            Ok(None) => None,
            Ok(Some(status)) => {
                tracing::warn!(%status, "supervision: browser child exited");
                Self::clear_dead(&mut state);
                Some(DeathSignal::Exited(status))
            }
            Err(e) => {
                // try_wait failing means child liveness is unknowable —
                // the wrapper treats that as a death verdict.
                tracing::warn!(error = %e, "supervision: try_wait failed");
                Self::clear_dead(&mut state);
                Some(DeathSignal::Exited(format!("try_wait failed: {e}")))
            }
        }
    }

    /// Marks dead without a reason — the daemon's shutdown path.
    pub async fn mark_dead(&self) {
        let mut state = self.inner.lock().await;
        Self::clear_dead(&mut state);
    }

    /// Shuts the browser child down (best effort) and clears the state.
    ///
    /// # Errors
    ///
    /// A shutdown failure is reported; the state is cleared regardless.
    pub async fn shutdown_browser(&self) -> Result<(), BackendError> {
        let mut state = self.inner.lock().await;
        if let Some(engine) = state.engine.take() {
            engine.handle.shutdown().await?;
        }
        Self::clear_dead(&mut state);
        Ok(())
    }

    /// Whether a live engine is present. (Named `has_live_engine`, not
    /// `is_live`: the [`BrowserBackend`] trait declares the latter with
    /// async-trait's boxing, and the two must not shadow.)
    #[must_use]
    pub async fn has_live_engine(&self) -> bool {
        self.inner.lock().await.engine.is_some()
    }

    /// Clears dead state. Caller holds the state lock.
    fn clear_dead(state: &mut BackendState) {
        state.engine = None;
        state.tabs.clear();
        state.refs.clear();
    }

    /// Brings an engine up if none is live. Also consumes a death verdict
    /// that arrived between supervision ticks: a channel flip while the
    /// engine is nominally live clears the state and relaunches.
    ///
    /// The state lock is held across the launch — commands serialize on
    /// this lock by design, which keeps recycle decisions race-free.
    async fn ensure_live(&self) -> Result<(), BackendError> {
        let mut state = self.inner.lock().await;
        if let Some(engine) = state.engine.as_ref() {
            let died =
                engine.death.has_changed().unwrap_or(false) && engine.death.borrow().is_some();
            if !died {
                return Ok(());
            }
            tracing::warn!("browser died between commands; recycling");
            Self::clear_dead(&mut state);
        }
        tracing::info!("launching chrome");
        let live = tokio::time::timeout(
            self.command_timeout,
            self.launcher.launch(&self.executable, &self.user_data_dir),
        )
        .await
        .map_err(|e| BackendError::Engine(format!("launch timed out: {e}")))?
        .map_err(|e| BackendError::Engine(e.to_string()))?;
        state.engine = Some(Engine {
            handle: live.handle,
            death: live.death,
            pages: HashMap::new(),
        });
        Ok(())
    }

    /// Runs one command under the per-command timeout. A command that
    /// reports [`BackendError::DeadBrowser`] (a dead transport judged from
    /// the error text) also clears the state: the verdict applies to the
    /// engine, not just the command. A timeout does not.
    async fn run<T, F>(&self, command: F) -> Result<T, BackendError>
    where
        F: std::future::Future<Output = Result<T, BackendError>> + Send,
    {
        let result = tokio::time::timeout(self.command_timeout, command).await;
        match result {
            Ok(Err(BackendError::DeadBrowser(reason))) => {
                let mut state = self.inner.lock().await;
                tracing::warn!(%reason, "command reported dead transport; marking dead");
                Self::clear_dead(&mut state);
                Err(BackendError::DeadBrowser(reason))
            }
            other => other.unwrap_or(Err(BackendError::Timeout)),
        }
    }

    /// Resolves a tab id to its page handle, minting the wrapper on first
    /// touch. Ensures the engine is live first.
    async fn page_for(&self, tab: &str) -> Result<Arc<dyn PageHandle>, BackendError> {
        let target = {
            let state = self.inner.lock().await;
            let entry = state
                .tabs
                .iter()
                .find(|t| t.public == tab)
                .ok_or_else(|| BackendError::UnknownTab(tab.to_owned()))?;
            entry.target.clone()
        };

        let mut state = self.inner.lock().await;
        let engine = state
            .engine
            .as_ref()
            .ok_or_else(|| BackendError::DeadBrowser("no live browser".to_owned()))?;
        if let Some(page) = engine.pages.get(&target) {
            return Ok(page.clone());
        }
        let page = engine
            .handle
            .get_page(&target)
            .await
            .map_err(|e| BackendError::Engine(format!("tab {tab} is gone: {e}")))?;
        if let Some(engine) = state.engine.as_mut() {
            engine.pages.insert(target, page.clone());
        }
        Ok(page)
    }

    /// Drops a tab's page wrapper and ref table.
    async fn forget_tab(&self, tab: &str) {
        let mut state = self.inner.lock().await;
        state.refs.remove(tab);
        if let Some(entry) = state.tabs.iter().find(|t| t.public == tab) {
            let target = entry.target.clone();
            if let Some(engine) = state.engine.as_mut() {
                engine.pages.remove(&target);
            }
        }
        state.tabs.retain(|t| t.public != tab);
    }

    /// Mints the next public tab id (`t<n>`).
    fn mint_tab_id(next: &mut u64) -> String {
        let id = format!("t{next}");
        *next += 1;
        id
    }

    /// Registers a freshly opened page as a tab.
    async fn register_tab(&self, target: TargetId, page: Arc<dyn PageHandle>) -> String {
        let mut state = self.inner.lock().await;
        let public = Self::mint_tab_id(&mut state.next_tab);
        if let Some(engine) = state.engine.as_mut() {
            engine.pages.insert(target.clone(), page);
        }
        state.tabs.push(MintedTab {
            public: public.clone(),
            target,
        });
        public
    }

    /// Reads a tab's current url and title through its page handle.
    async fn tab_info(&self, entry: &MintedTab) -> TabInfo {
        let page = {
            let state = self.inner.lock().await;
            state
                .engine
                .as_ref()
                .and_then(|e| e.pages.get(&entry.target))
                .cloned()
        };
        let Some(page) = page else {
            return TabInfo {
                tab: entry.public.clone(),
                url: None,
                title: None,
            };
        };
        let url = page.url().await.ok().flatten();
        let title = page.title().await.ok().flatten();
        TabInfo {
            tab: entry.public.clone(),
            url,
            title,
        }
    }
}

/// The script-side shape a snapshot's evaluate returns.
#[derive(Debug, Deserialize)]
struct ScriptedRef {
    tag: String,
    #[serde(rename = "id")]
    html_id: Option<String>,
    text: Option<String>,
    value: Option<String>,
    x: f64,
    y: f64,
    visible: bool,
}

/// The element selector the snapshot and the click path share.
const INTERACTIVE_SELECTOR: &str = "a, button, input, textarea, select, [role], [onclick], [contenteditable=\"true\"], [contenteditable=\"\"]";

/// Whether an engine error text names a dead transport (a death verdict)
/// rather than an ordinary command failure.
fn is_death_text(text: &str) -> bool {
    let lowered = text.to_lowercase();
    lowered.contains("closed")
        || lowered.contains("disconnected")
        || lowered.contains("websocket")
        || lowered.contains("no response")
}

/// Maps a chaser engine error onto the seam's error type, judging death
/// from the error text (a dead transport reads as "closed"/"disconnected").
/// ChaserPage methods report `anyhow` errors; `Browser` methods report
/// `CdpError`; both render through `Display`, so the mapper takes `impl`.
fn map_engine_error(e: impl std::fmt::Display) -> BackendError {
    let text = e.to_string();
    if is_death_text(&text) {
        BackendError::DeadBrowser(text)
    } else {
        BackendError::Engine(text)
    }
}

#[async_trait::async_trait]
impl<L: EngineLauncher> BrowserBackend for ChaserBackend<L> {
    async fn open_tab(&self, url: Option<&str>) -> Result<TabInfo, BackendError> {
        self.ensure_live().await?;
        // about:blank first, then a real navigation: `new_page` on a URL
        // blocks until that page's load event, which couples tab creation
        // to page health; blank creation is instant and the navigate call
        // carries its own failure semantics.
        let (target, page) = {
            let state = self.inner.lock().await;
            let engine = state
                .engine
                .as_ref()
                .ok_or_else(|| BackendError::DeadBrowser("no live browser".to_owned()))?;
            engine.handle.new_page("about:blank").await?
        };
        let public = self.register_tab(target, page.clone()).await;
        if let Some(url) = url {
            self.run(page.goto(url)).await?;
        }
        let info_url = page.url().await.ok().flatten();
        let info_title = page.title().await.ok().flatten();
        Ok(TabInfo {
            tab: public,
            url: info_url,
            title: info_title,
        })
    }

    async fn list_tabs(&self) -> Result<Vec<TabInfo>, BackendError> {
        let entries: Vec<MintedTab> = {
            let state = self.inner.lock().await;
            state.tabs.clone()
        };
        let mut infos = Vec::with_capacity(entries.len());
        for entry in &entries {
            infos.push(self.tab_info(entry).await);
        }
        Ok(infos)
    }

    async fn navigate(&self, tab: &str, url: &str) -> Result<TabInfo, BackendError> {
        let page = self.page_for(tab).await?;
        let url = url.to_owned();
        self.run(async {
            page.goto(&url).await?;
            Ok(TabInfo {
                tab: tab.to_owned(),
                url: Some(url),
                title: None,
            })
        })
        .await
    }

    async fn snapshot(&self, tab: &str) -> Result<Snapshot, BackendError> {
        let page = self.page_for(tab).await?;
        // One script does everything: refs from the live DOM with geometry,
        // via the fork's isolated-world path (no Runtime.enable). The
        // result arrives double-encoded (the script JSON.stringify's its
        // payload), so decode the string, then the refs.
        let script = format!(
            r#"(() => {{
                const nodes = Array.from(document.querySelectorAll('{sel}'));
                return JSON.stringify(nodes.slice(0, 400).map((el) => {{
                    const r = el.getBoundingClientRect();
                    return {{
                        tag: el.tagName.toLowerCase(),
                        id: el.id || null,
                        text: (el.innerText || el.value || el.placeholder || el.getAttribute('aria-label') || '').trim().slice(0, 120) || null,
                        value: (el.value !== undefined && el.tagName === 'INPUT') ? String(el.value).slice(0, 120) : null,
                        x: Math.round(r.x + r.width / 2),
                        y: Math.round(r.y + r.height / 2),
                        visible: r.width > 0 && r.height > 0,
                    }};
                }}));
            }})()"#,
            sel = INTERACTIVE_SELECTOR.replace('"', "'"),
        );
        let raw = self.run(page.evaluate_stealth(&script)).await?;
        let entries: Vec<ScriptedRef> = match raw {
            Some(serde_json::Value::String(encoded)) => serde_json::from_str(&encoded)
                .map_err(|e| BackendError::Engine(format!("snapshot decode failed: {e}")))?,
            _ => Vec::new(),
        };

        let url = page.url().await.ok().flatten();
        let title = page.title().await.ok().flatten();

        // Mint refs, cache geometry, render the tree.
        let mut refs = Vec::with_capacity(entries.len());
        let mut ref_entries = Vec::with_capacity(entries.len());
        let mut tree = String::new();
        tree.push_str("# ");
        tree.push_str(title.as_deref().unwrap_or("(untitled)"));
        tree.push('\n');
        tree.push_str("# ");
        tree.push_str(url.as_deref().unwrap_or(""));
        tree.push_str("\n\n");
        for (i, entry) in entries.iter().enumerate() {
            let id = format!("e{}", i + 1);
            let text = entry.text.clone().filter(|t| !t.is_empty());
            tree.push_str("- [");
            tree.push_str(&id);
            tree.push_str("] <");
            tree.push_str(&entry.tag);
            let anchor = entry
                .html_id
                .as_ref()
                .map_or_else(String::new, |h| format!("#{h}"));
            tree.push_str(&anchor);
            tree.push_str("> ");
            tree.push_str(text.as_deref().unwrap_or(""));
            if entry.visible {
                tree.push('\n');
            } else {
                tree.push_str(" [hidden]\n");
            }
            ref_entries.push(RefEntry {
                id: id.clone(),
                center: (entry.x, entry.y),
            });
            refs.push(SnapshotRef {
                id,
                tag: entry.tag.clone(),
                text: entry.text.clone(),
                html_id: entry.html_id.clone(),
                value: entry.value.clone(),
            });
        }
        {
            let mut state = self.inner.lock().await;
            state.refs.insert(tab.to_owned(), ref_entries);
        }
        Ok(Snapshot {
            tab: tab.to_owned(),
            url,
            title,
            refs,
            tree,
        })
    }

    async fn click(&self, tab: &str, reference: &str) -> Result<(), BackendError> {
        let page = self.page_for(tab).await?;
        let center = {
            let state = self.inner.lock().await;
            let entry = state
                .refs
                .get(tab)
                .and_then(|refs| refs.iter().find(|r| r.id == reference))
                .ok_or_else(|| BackendError::StaleRef(reference.to_owned()))?;
            entry.center
        };
        // Scroll the element into view first and re-read its center: the
        // cached geometry is from snapshot time and may have shifted.
        // The index positions into the same selector order the snapshot
        // used, so ref e<n> names the n-th interactive element.
        let index = reference
            .trim_start_matches('e')
            .parse::<usize>()
            .map_err(|e| BackendError::Engine(format!("bad ref `{reference}`: {e}")))?
            .checked_sub(1)
            .ok_or_else(|| BackendError::StaleRef(reference.to_owned()))?;
        let scroll_script = format!(
            r#"(() => {{
                const els = document.querySelectorAll('{sel}');
                const el = els[{index}];
                if (!el) return 'missing';
                el.scrollIntoView({{block: 'center'}});
                const r = el.getBoundingClientRect();
                return JSON.stringify({{x: Math.round(r.x + r.width/2), y: Math.round(r.y + r.height/2)}});
            }})()"#,
            sel = INTERACTIVE_SELECTOR.replace('"', "'"),
            index = index,
        );
        let fresh = self.run(page.evaluate_stealth(&scroll_script)).await?;
        let center = match fresh {
            Some(serde_json::Value::String(encoded)) => {
                let parsed: serde_json::Value = serde_json::from_str(&encoded)
                    .map_err(|e| BackendError::Engine(format!("scroll decode failed: {e}")))?;
                let x = parsed
                    .get("x")
                    .and_then(serde_json::Value::as_f64)
                    .unwrap_or(center.0);
                let y = parsed
                    .get("y")
                    .and_then(serde_json::Value::as_f64)
                    .unwrap_or(center.1);
                (x, y)
            }
            // 'missing' (or a null): the DOM shifted under the ref.
            _ => return Err(BackendError::StaleRef(reference.to_owned())),
        };
        self.run(page.click_human(center.0, center.1)).await
    }

    async fn type_text(&self, tab: &str, reference: &str, text: &str) -> Result<(), BackendError> {
        // Click to focus, then human-type.
        self.click(tab, reference).await?;
        let page = self.page_for(tab).await?;
        let text = text.to_owned();
        self.run(async move { page.type_text(&text).await }).await
    }

    async fn press_key(&self, tab: &str, key: &str) -> Result<(), BackendError> {
        let page = self.page_for(tab).await?;
        let key = key.to_owned();
        self.run(async move { page.press_key(&key).await }).await
    }

    async fn evaluate(
        &self,
        tab: &str,
        script: &str,
    ) -> Result<Option<serde_json::Value>, BackendError> {
        let page = self.page_for(tab).await?;
        let script = script.to_owned();
        self.run(async move { page.evaluate_stealth(&script).await })
            .await
    }

    async fn screenshot(&self, tab: &str) -> Result<Vec<u8>, BackendError> {
        let page = self.page_for(tab).await?;
        self.run(page.screenshot()).await
    }

    async fn wait(&self, tab: &str, condition: &WaitCondition) -> Result<(), BackendError> {
        match condition {
            WaitCondition::Navigation | WaitCondition::NetworkIdle => {
                let page = self.page_for(tab).await?;
                // A completed load is the navigation settle, read through
                // the isolated-world path: the handler-side navigation
                // watcher reads Network events the fork no longer enables.
                self.run(async move {
                    loop {
                        let ready = page.evaluate_stealth("document.readyState").await?;
                        if ready == Some(serde_json::Value::String("complete".to_owned())) {
                            return Ok(());
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    }
                })
                .await
            }
            WaitCondition::Text(needle) => {
                let page = self.page_for(tab).await?;
                let script = format!(
                    "document.body ? document.body.innerText.includes({}) : false",
                    serde_json::to_string(needle)
                        .map_err(|e| BackendError::Engine(format!("encode failed: {e}")))?,
                );
                self.run(async move {
                    loop {
                        let found = page.evaluate_stealth(&script).await?;
                        if found == Some(serde_json::Value::Bool(true)) {
                            return Ok(());
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    }
                })
                .await
            }
            WaitCondition::Ms(ms) => {
                let ms = *ms;
                self.page_for(tab).await?;
                self.run(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
                    Ok(())
                })
                .await
            }
        }
    }

    async fn close_tab(&self, tab: &str) -> Result<(), BackendError> {
        let page = self.page_for(tab).await?;
        self.run(page.close()).await?;
        self.forget_tab(tab).await;
        Ok(())
    }

    async fn is_live(&self) -> bool {
        ChaserBackend::has_live_engine(self).await
    }
}

#[cfg(feature = "daemon")]
mod chaser {
    //! The production impls: chaser-oxide wrapped behind the seam's
    //! abstract handles. Nothing in here escapes the module.

    use super::{BackendError, EngineHandle, PageHandle, map_engine_error};
    use chaser_oxide::cdp::browser_protocol::target::TargetId;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    /// The production engine handle. The browser mutex is locked only for
    /// browser-level operations; page work runs on cloned page handles.
    pub(super) struct ChaserEngineHandle {
        browser: Arc<Mutex<chaser_oxide::Browser>>,
    }

    impl ChaserEngineHandle {
        /// Wraps a launched browser. Construction stays inside the
        /// module so the field never leaks raw chaser ownership.
        pub(super) fn wrap(browser: Arc<Mutex<chaser_oxide::Browser>>) -> Self {
            Self { browser }
        }
    }

    #[async_trait::async_trait]
    impl EngineHandle for ChaserEngineHandle {
        async fn new_page(
            &self,
            url: &str,
        ) -> Result<(TargetId, Arc<dyn PageHandle>), BackendError> {
            let browser = self.browser.lock().await;
            let page = browser
                .new_page(chaser_oxide::cdp::browser_protocol::target::CreateTargetParams::new(url))
                .await
                .map_err(map_engine_error)?;
            let target = page.target_id().clone();
            Ok((target, Arc::new(ChaserPageHandle::new(page))))
        }

        async fn get_page(&self, target: &TargetId) -> Result<Arc<dyn PageHandle>, BackendError> {
            let browser = self.browser.lock().await;
            let page = browser
                .get_page(target.clone())
                .await
                .map_err(map_engine_error)?;
            Ok(Arc::new(ChaserPageHandle::new(page)))
        }

        async fn try_wait(&self) -> std::io::Result<Option<String>> {
            // try_wait takes &mut Browser (child state mutation); the other
            // browser-level calls ride &self through the command sender.
            let mut browser = self.browser.lock().await;
            Ok(browser.try_wait()?.map(|status| status.to_string()))
        }

        async fn shutdown(&self) -> Result<(), BackendError> {
            let mut browser = self.browser.lock().await;
            // kill is best effort: a browser already gone reports an error
            // that must not mask the shutdown itself.
            let _ = browser.kill().await;
            Ok(())
        }
    }

    /// The production page handle: a chaser stealth page. Every eval on
    /// this handle is the isolated-world path; the title especially is
    /// read here rather than through `Page::get_title`, which triggers
    /// `Runtime.enable`.
    struct ChaserPageHandle {
        page: chaser_oxide::ChaserPage,
    }

    impl ChaserPageHandle {
        fn new(page: chaser_oxide::Page) -> Self {
            Self {
                page: chaser_oxide::ChaserPage::new(page),
            }
        }
    }

    #[async_trait::async_trait]
    impl PageHandle for ChaserPageHandle {
        async fn goto(&self, url: &str) -> Result<(), BackendError> {
            self.page
                .goto(url)
                .await
                .map(drop)
                .map_err(map_engine_error)
        }

        async fn url(&self) -> Result<Option<String>, BackendError> {
            self.page.url().await.map_err(map_engine_error)
        }

        async fn title(&self) -> Result<Option<String>, BackendError> {
            match self
                .page
                .evaluate_stealth("document.title")
                .await
                .map_err(map_engine_error)?
            {
                Some(serde_json::Value::String(s)) if !s.is_empty() => Ok(Some(s)),
                _ => Ok(None),
            }
        }

        async fn evaluate_stealth(
            &self,
            script: &str,
        ) -> Result<Option<serde_json::Value>, BackendError> {
            self.page
                .evaluate_stealth(script)
                .await
                .map_err(map_engine_error)
        }

        async fn click_human(&self, x: f64, y: f64) -> Result<(), BackendError> {
            self.page.click_human(x, y).await.map_err(map_engine_error)
        }

        async fn type_text(&self, text: &str) -> Result<(), BackendError> {
            self.page.type_text(text).await.map_err(map_engine_error)
        }

        async fn press_key(&self, key: &str) -> Result<(), BackendError> {
            self.page.press_key(key).await.map_err(map_engine_error)
        }

        async fn screenshot(&self) -> Result<Vec<u8>, BackendError> {
            self.page
                .raw_page()
                .screenshot(
                    chaser_oxide::cdp::browser_protocol::page::CaptureScreenshotParams::builder()
                        .format(
                            chaser_oxide::cdp::browser_protocol::page::CaptureScreenshotFormat::Png,
                        )
                        .build(),
                )
                .await
                .map_err(map_engine_error)
        }

        async fn close(&self) -> Result<(), BackendError> {
            self.page
                .raw_page()
                .clone()
                .close()
                .await
                .map(drop)
                .map_err(map_engine_error)
        }
    }
}

/// The production launcher: launches Chrome headed with the fork's curated
/// args, spawns the handler-driver task, and reports the stream's death
/// through the engine's channel.
pub struct ChaserLauncher;

#[async_trait::async_trait]
impl EngineLauncher for ChaserLauncher {
    async fn launch(
        &self,
        executable: &std::path::Path,
        user_data_dir: &std::path::Path,
    ) -> Result<LiveEngine, BackendError> {
        let config = chaser_oxide::BrowserConfig::builder()
            .user_data_dir(user_data_dir)
            .chrome_executable(executable)
            // Headed — never headless (S1–S7 signals, focus texture).
            .with_head()
            .build()
            .map_err(|e| BackendError::Engine(format!("browser config rejected: {e}")))?;

        let (browser, handler) = chaser_oxide::Browser::launch(config)
            .await
            .map_err(|e| BackendError::Engine(format!("chrome launch failed: {e}")))?;

        let (death_tx, death_rx) = tokio::sync::watch::channel(None::<String>);
        // Drive the handler stream for the engine's whole life. Its error
        // or unexpected end is the handler death signal; nothing else
        // consumes the stream.
        tokio::spawn(async move {
            let mut handler = handler;
            while let Some(item) = futures::StreamExt::next(&mut handler).await {
                if let Err(e) = item {
                    let _ = death_tx.send(Some(e.to_string()));
                    return;
                }
            }
            let _ = death_tx.send(Some("handler stream ended unexpectedly".to_owned()));
        });

        Ok(LiveEngine {
            handle: Box::new(chaser::ChaserEngineHandle::wrap(Arc::new(Mutex::new(
                browser,
            )))),
            death: death_rx,
        })
    }
}
