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
    async fn goto(&self, url: String) -> Result<(), BackendError>;

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
        script: String,
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
    async fn new_page(&self, url: String) -> Result<(TargetId, Arc<dyn PageHandle>), BackendError>;

    /// Resolves (or re-attaches) the page handle for a target id.
    ///
    /// # Errors
    ///
    /// A target that no longer exists maps onto [`BackendError::Engine`].
    async fn get_page(&self, target: &TargetId) -> Result<Arc<dyn PageHandle>, BackendError>;

    /// Lists every live target id with its type and URL — the raw
    /// target inventory the OOPIF frame path filters. Includes
    /// `type: "iframe"` (out-of-process) targets that `pages()` hides.
    ///
    /// # Errors
    ///
    /// Engine failures map onto [`BackendError`].
    async fn list_targets(&self) -> Result<Vec<crate::surface::TargetInfo>, BackendError>;

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

    /// Human-clicks at viewport CSS-pixel coordinates. Coordinates are
    /// clamped to the page viewport before dispatch; the click rides the
    /// same trusted-input path as ref clicks.
    ///
    /// # Errors
    ///
    /// Backend errors name the failure for tool-level rendering.
    async fn click_at(&self, tab: &str, x: f64, y: f64) -> Result<(), BackendError>;

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

    /// Evaluates a script inside a cross-origin frame ref (`g<k>`) of
    /// the tab, on that frame's own CDP session (stealth path).
    ///
    /// # Errors
    ///
    /// Backend errors name the failure for tool-level rendering.
    async fn evaluate_in_frame(
        &self,
        tab: &str,
        frame: &str,
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
    /// Consecutive input-class command timeouts. Chromium's Input domain
    /// can wedge (stop acking dispatches) with no death signal anywhere —
    /// evaluate keeps working, only mouse commands hang — so consecutive
    /// input timeouts are the only observable wedge signature, and the
    /// watchdog treats reaching [`INPUT_TIMEOUT_WEDGE_LIMIT`] as a death
    /// verdict: recycle instead of hanging forever.
    input_timeouts: u32,
}

/// Consecutive input-class command timeouts that count as a wedged Input
/// domain. Two, not one: a single slow dispatch (a 5s renderer stall under
/// a 10s budget) recovers on its own; a second means the input path is
/// stuck, not slow.
const INPUT_TIMEOUT_WEDGE_LIMIT: u32 = 2;

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

/// One cached interactive element: what the click path needs. Display
/// metadata lives in the snapshot response, not here.
struct RefEntry {
    id: String,
    /// Viewport-absolute center.
    center: (f64, f64),
    /// The owning same-origin iframe: `Some("f1")` for frame refs, and
    /// the viewport-absolute rect of the iframe element in the parent —
    /// the click path re-resolves the in-frame node and adds the rect.
    frame: Option<FrameAnchor>,
    /// The owning cross-origin iframe target: `Some` for `g<k>` refs.
    /// The click dispatches on the iframe's own CDP session (a top-frame
    /// dispatch never routes into an out-of-process frame).
    target_frame: Option<FrameTarget>,
}

/// The parent-side anchor of a same-origin iframe: the iframe element's
/// index into the main-frame frame-element list (document order — the
/// same order the snapshot walked), which the click path re-resolves
/// live (rect and same-origin access happen at click time).
#[derive(Clone, Copy)]
struct FrameAnchor {
    /// Index into `document.querySelectorAll("iframe,frame")` order.
    iframe_index: usize,
}

/// The anchor of a cross-origin (OOPIF) frame ref: the `g<k>` label and
/// the CDP target id it resolved to at snapshot time. The target id is
/// re-validated at click time — OOPIF targets die and re-create on every
/// navigation, so a cached id is a hint, never a truth.
#[derive(Debug, Clone)]
struct FrameTarget {
    /// The `g<k>` label minted by the snapshot.
    label: String,
    /// The CDP target id at snapshot time.
    target_id: String,
    /// The iframe element's index into the top document's frame list —
    /// how a `g<k>` on screen maps to the target whose URL matches the
    /// placeholder's src (src-equal groups match by order). Kept for
    /// future parent-rect remapping; target-session dispatch needs only
    /// the target id.
    #[allow(dead_code, reason = "reserved for parent-rect remapping")]
    iframe_index: usize,
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
                input_timeouts: 0,
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
        F: std::future::Future<Output = Result<T, BackendError>> + Send + 'static,
        T: Send + 'static,
    {
        // The future is spawned, never dropped: a timeout abandons the
        // *await*, not the dispatch. Dropping a mouse click mid-sequence
        // strands Chromium with a pressed button and wedges all further
        // input (the diagnosed wedge in the input-wedge investigation),
        // so input commands in particular must always run to completion.
        let handle = tokio::spawn(command);
        let result = match tokio::time::timeout(self.command_timeout, handle).await {
            Ok(joined) => joined
                .unwrap_or_else(|e| Err(BackendError::Engine(format!("command task failed: {e}")))),
            Err(_) => Err(BackendError::Timeout),
        };
        match result {
            Ok(value) => Ok(value),
            Err(BackendError::DeadBrowser(reason)) => {
                let mut state = self.inner.lock().await;
                tracing::warn!(%reason, "command reported dead transport; marking dead");
                Self::clear_dead(&mut state);
                Err(BackendError::DeadBrowser(reason))
            }
            Err(e) => Err(e),
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

/// The script-side shape a snapshot's evaluate returns. Frame nodes
/// carry the frame label and their in-frame eid; iframe placeholder
/// rows carry `clickable:false` and the iframe element's list index.
#[derive(Debug, Deserialize)]
struct ScriptedRef {
    tag: String,
    #[serde(rename = "id")]
    html_id: Option<String>,
    text: Option<String>,
    value: Option<String>,
    /// The ref id within its frame (`e12`); the backend prefixes frame
    /// rows as `f<k>:e12`.
    #[serde(default)]
    eid: Option<String>,
    /// The frame label the script assigned (`f1`), or `null` on top.
    #[serde(default)]
    frame: Option<String>,
    /// The iframe element's index into the frame list (placeholder rows).
    #[serde(default)]
    iframe_index: Option<usize>,
    /// Viewport-absolute center x.
    x: f64,
    /// Viewport-absolute center y.
    y: f64,
    visible: bool,
    /// `false` only for iframe placeholder rows.
    #[serde(default = "default_clickable")]
    clickable: bool,
}

/// Serde default for the script's clickable flag (element rows are
/// clickable by default; only placeholders set it false).
fn default_clickable() -> bool {
    true
}

/// The element selector the snapshot and the click path share. The
/// attribute values are double-quoted to sit inside the scripts'
/// single-quoted `querySelectorAll('…')` string literals; the earlier
/// single-quoted form collided with those literals and made every
/// snapshot a `SyntaxError` (recorded failure mode).
const INTERACTIVE_SELECTOR: &str = "a, button, input, textarea, select, [role], [onclick], [contenteditable=\"true\"], [contenteditable=\"\"]";

/// The in-frame interactive walk the snapshot runs inside a matched
/// OOPIF target and the click path re-runs for a fresh center. Returns
/// a JSON array of the same row shape the main snapshot script emits,
/// with viewport-geometry already iframe-local (the dispatcher on the
/// iframe's session takes iframe-viewport coordinates directly).
const OOPIF_WALK_TAIL: &str = r#"
                const els = document.querySelectorAll(SEL);
                const out = [];
                let n = 0;
                for (const el of els) {
                    n += 1;
                    const r = el.getBoundingClientRect();
                    if (r.width <= 0 || r.height > 20000) continue;
                    out.push({
                        tag: el.tagName.toLowerCase(),
                        id: el.id || null,
                        text: (el.innerText || el.value || el.placeholder || el.getAttribute('aria-label') || '').trim().slice(0, 120) || null,
                        value: (el.value !== undefined && el.tagName === 'INPUT') ? String(el.value).slice(0, 120) : null,
                        eid: 'e' + n,
                        x: Math.round(r.x + r.width / 2),
                        y: Math.round(r.y + r.height / 2),
                        visible: r.width > 0 && r.height > 0,
                    });
                }
                return JSON.stringify(out);"#;

/// Whether a script is statement-shaped (needs the IIFE wrap) rather
/// than a bare expression. Conservative by design: only an explicit
/// `return` (or `throw`) outside string literals triggers the wrap — a
/// false positive costs nothing for `return`-scripts (the wrap is what
/// makes them legal) but would swallow an expression's value.
#[must_use]
/// Whether a script already ends with an IIFE invocation — `})()`,
/// `})();`, `)()`, or `)();` (allowing trailing whitespace/semicolons) —
/// scanned outside string literals and comments so a string ending in
/// `})()` doesn't trip it. Such a script is an expression: its
/// completion value is already the return value, and wrapping it in
/// another function would swallow that value.
pub(crate) fn script_ends_with_iife_call(script: &str) -> bool {
    let trimmed = script.trim_end().trim_end_matches(';').trim_end();
    let chars: Vec<char> = trimmed.chars().collect();
    // Walk the tail backwards, skipping string literals (no multi-line
    // comment tracking needed for a tail scan; a comment before the tail
    // would only appear between code and the `)()` which we handle by
    // trimming — a trailing comment case falls back to no-detection,
    // which is safe: wrapping it would still be *correct* for plain
    // statements but wrong for IIFEs, and trailing comments after an
    // IIFE are vanishingly rare).
    let mut i = chars.len();
    let mut depth = 0i32;
    let mut in_string: Option<char> = None;
    while i > 0 {
        i -= 1;
        let Some(&c) = chars.get(i) else {
            return false;
        };
        if let Some(q) = in_string {
            if c == '\\' {
                if i == 0 {
                    return false;
                }
                i -= 1;
            } else if c == q {
                in_string = None;
            }
            continue;
        }
        match c {
            '"' | '\'' | '`' => in_string = Some(c),
            ')' => {
                depth += 1;
            }
            '(' => {
                depth -= 1;
                // depth 0 from the scan start means this `(` opens the
                // trailing call itself. The character before it must
                // close a callee: `}` for a block-body arrow/function
                // (`(() => { … })()`) or `)` for a parenthesized callee
                // (`(function () { … })()`, `(expr)()`, `f()()`). Any
                // such tail is a call expression — its completion value
                // is already the script's value.
                if depth == 0 {
                    let before = if i == 0 {
                        ' '
                    } else {
                        chars.get(i - 1).copied().unwrap_or(' ')
                    };
                    return before == '}' || before == ')';
                }
                if depth < 0 {
                    return false;
                }
            }
            _ => {}
        }
    }
    false
}

pub(crate) fn script_looks_like_statements(script: &str) -> bool {
    // Walk with a tiny state machine: track string/template/comment
    // context so "returned" inside a string literal doesn't match.
    let chars: Vec<char> = script.chars().collect();
    let mut i = 0usize;
    let mut in_string: Option<char> = None;
    let mut in_line_comment = false;
    let mut in_block_comment = false;
    let mut prev_word_boundary = true;
    while let Some(&c) = chars.get(i) {
        if let Some(q) = in_string {
            if c == '\\' {
                i += 2;
                continue;
            }
            if c == q {
                in_string = None;
            }
            i += 1;
            continue;
        }
        if in_line_comment {
            if c == '\n' {
                in_line_comment = false;
            }
            i += 1;
            continue;
        }
        if in_block_comment {
            if c == '*' && chars.get(i + 1) == Some(&'/') {
                in_block_comment = false;
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        match c {
            '"' | '\'' | '`' => in_string = Some(c),
            '/' if chars.get(i + 1) == Some(&'/') => in_line_comment = true,
            '/' if chars.get(i + 1) == Some(&'*') => in_block_comment = true,
            _ => {
                let is_word = c.is_alphanumeric() || c == '_' || c == '$';
                if is_word && prev_word_boundary {
                    // Candidate keyword start: read the word.
                    let word: String = chars
                        .get(i..)
                        .unwrap_or(&[])
                        .iter()
                        .take_while(|ch| ch.is_alphanumeric() || **ch == '_' || **ch == '$')
                        .collect();
                    if word == "return" || word == "throw" {
                        return true;
                    }
                    i += word.chars().count();
                    prev_word_boundary = false;
                    continue;
                }
                prev_word_boundary = !is_word;
            }
        }
        i += 1;
    }
    false
}

/// Whether an engine error text names a dead transport (a death verdict)
/// rather than an ordinary command failure.
fn is_death_text(text: &str) -> bool {
    let lowered = text.to_lowercase();
    lowered.contains("closed")
        || lowered.contains("disconnected")
        || lowered.contains("websocket")
        || lowered.contains("no response")
}

/// Whether a placeholder src and a target URL name the same document:
/// scheme+host+path equality, query ignored (consent frames ride URLs
/// whose query string mutates per page load; the path identifies the
/// frame). An empty src (srcdoc) never matches.
#[must_use]
pub(crate) fn urls_share_path(target_url: &str, placeholder_src: &str) -> bool {
    let path_of = |u: &str| -> Option<String> {
        let rest = u.split_once("://")?.1;
        let after = match rest.split_once('/') {
            Some((_, tail)) => tail,
            None => "",
        };
        Some(after.split('?').next().unwrap_or(after).to_owned())
    };
    match (path_of(target_url), path_of(placeholder_src)) {
        // An authority-only src ("https://cdn.example") has an empty
        // path and matches the target's root "/" — an entirely empty
        // path pair never matches.
        (Some(a), Some(b)) => {
            if b.is_empty() {
                a == "/"
            } else {
                a.starts_with(b.as_str())
            }
        }
        _ => false,
    }
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
            engine.handle.new_page("about:blank".to_owned()).await?
        };
        let public = self.register_tab(target, page.clone()).await;
        if let Some(url) = url {
            let page = page.clone();
            let url = url.to_owned();
            self.run(async move { page.goto(url).await }).await?;
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
        let tab_label = tab.to_owned();
        let page = page.clone();
        self.run(async move {
            page.goto(url.clone()).await?;
            Ok(TabInfo {
                tab: tab_label,
                url: Some(url),
                title: None,
            })
        })
        .await
    }

    async fn snapshot(&self, tab: &str) -> Result<Snapshot, BackendError> {
        let page = self.page_for(tab).await?;
        // One script does everything: refs from the live DOM with
        // geometry, same-origin iframe recursion (depth-capped), shadow
        // DOM piercing, and iframe placeholder rows for frame discovery
        // — all via the fork's isolated-world path (no Runtime.enable).
        // The result arrives double-encoded (the script JSON.stringify's
        // its payload), so decode the string, then the rows.
        let script = format!(
            r#"(() => {{
                const SEL = '{INTERACTIVE_SELECTOR}';
                const MAX_DEPTH = 3;
                const out = [];

                function collect(doc, ox, oy, frame, iframeIndex) {{
                    // Interactive elements of this document, in document
                    // order, then its shadow roots. The row cap only
                    // bounds element rows; frame discovery happens in
                    // collectAll regardless of this cap.
                    let nodes = [];
                    try {{ nodes = Array.from(doc.querySelectorAll(SEL)); }} catch (e) {{ nodes = []; }}
                    for (const el of nodes) {{
                        if (out.length >= 400) return;
                        const r = el.getBoundingClientRect();
                        out.push({{
                            tag: el.tagName.toLowerCase(),
                            id: el.id || null,
                            text: (el.innerText || el.value || el.placeholder || el.getAttribute('aria-label') || '').trim().slice(0, 120) || null,
                            value: (el.value !== undefined && el.tagName === 'INPUT') ? String(el.value).slice(0, 120) : null,
                            frame: frame,
                            eid: null,
                            iframe_index: iframeIndex,
                            x: Math.round(ox + r.x + r.width / 2),
                            y: Math.round(oy + r.y + r.height / 2),
                            visible: r.width > 0 && r.height > 0,
                            clickable: true,
                        }});
                    }}

                    // Shadow DOM: a shadow root is a DocumentFragment
                    // whose querySelectorAll covers slotted content;
                    // fragment rects are already viewport-relative, so
                    // the frame's ox/oy passes straight through.
                    let hosts = [];
                    try {{ hosts = Array.from(doc.querySelectorAll('*')); }} catch (e) {{ hosts = []; }}
                    for (const host of hosts) {{
                        if (host.shadowRoot) {{
                            collect(host.shadowRoot, ox, oy, frame, iframeIndex);
                            if (out.length >= 400) return;
                        }}
                    }}
                }}

                // Each same-origin child frame gets a label (`c1`, `c2`,
                // … by discovery order), carried on its rows so ref ids
                // group per frame; the child's click anchor is the index
                // of its iframe element in the TOP document's frame list
                // (nested frames resolve through the top list too —
                // `contentDocument` chains from the top document).
                const childAnchor = {{}};

                function collectAll(doc, ox, oy, frame, iframeIndex, depth) {{
                    // Frame discovery first — placeholders and same-origin
                    // recursion happen even when this document's element
                    // rows hit the cap — then this document's elements.
                    if (depth >= MAX_DEPTH) return;
                    let frames = [];
                    try {{ frames = Array.from(doc.querySelectorAll('iframe, frame')); }} catch (e) {{ frames = []; }}
                    if (out.length < 400) {{
                        for (const fr of frames) {{
                            const frRect = fr.getBoundingClientRect();
                            // Placeholder row: every frame is visible to
                            // the snapshot even when cross-origin (content
                            // then unreachable — a matched OOPIF target
                            // carries the cross-origin refs). The row's
                            // iframe_index names THIS document's frame
                            // list position when the placeholder itself
                            // is top-level (its parent anchor is null) —
                            // the cross-origin resolver keys off it.
                            out.push({{
                                tag: 'iframe',
                                id: fr.id || null,
                                text: (fr.getAttribute('src') || '(srcdoc)').slice(0, 120),
                                value: null,
                                frame: frame,
                                eid: null,
                                iframe_index: (iframeIndex === null) ? frames.indexOf(fr) : iframeIndex,
                                x: Math.round(ox + frRect.x + frRect.width / 2),
                                y: Math.round(oy + frRect.y + frRect.height / 2),
                                visible: frRect.width > 0 && frRect.height > 0,
                                clickable: false,
                            }});
                        }}
                    }}
                    collect(doc, ox, oy, frame, iframeIndex);
                    // No cap check here: the recursion below must run
                    // even when this document saturated the element cap
                    // (collect re-checks the cap per push, so the worst
                    // case is bounded work, not unbounded rows).
                    for (const fr of frames) {{
                        // Same-origin recursion: cross-origin access throws.
                        const frRect = fr.getBoundingClientRect();
                        let child = null;
                        try {{ child = fr.contentDocument; }} catch (e) {{ child = null; }}
                        if (child && depth + 1 < MAX_DEPTH) {{
                            // The click anchor of a top-level child frame
                            // is its index in this document's frame list;
                            // nested frames keep their top-level ancestor's
                            // anchor (the chain walks contentDocument from
                            // the top).
                            const anchor = (iframeIndex === null) ? frames.indexOf(fr) : iframeIndex;
                            const label = 'c' + (Object.keys(childAnchor).length + 1);
                            childAnchor[label] = anchor;
                            const childFrame = (iframeIndex === null) ? label : frame;
                            collectAll(child, ox + frRect.x, oy + frRect.y, childFrame, anchor, depth + 1);
                        }}
                    }}
                }}

                collectAll(document, 0, 0, null, null, 0);

                // Assign ref ids per frame: top-frame rows become e1..;
                // rows inside a same-origin child frame become f<k>:e1..
                // (k by first mention) and carry the frame's click anchor
                // — the iframe element's index in the TOP document's
                // frame list, which the click path re-resolves.
                const counters = {{}};
                const frameNumbers = {{}};
                let topCount = 0;
                for (const row of out) {{
                    if (!row.clickable) {{
                        row.eid = null;
                        continue;
                    }}
                    if (row.frame === null || row.frame === undefined) {{
                        topCount += 1;
                        row.eid = 'e' + topCount;
                        row.iframe_index = null;
                        continue;
                    }}
                    // Assign each distinct frame label a number by first
                    // element-row mention order.
                    if (!(row.frame in frameNumbers)) {{
                        frameNumbers[row.frame] = Object.keys(frameNumbers).length + 1;
                    }}
                    const k = frameNumbers[row.frame];
                    counters[row.frame] = (counters[row.frame] || 0) + 1;
                    // Read the counter BEFORE rewriting row.frame, and
                    // look the anchor up by the original label too —
                    // both keyed by 'c<n>', not the 'f<n>' this loop
                    // is about to write.
                    const n = counters[row.frame];
                    const anchor = childAnchor[row.frame];
                    row.frame = 'f' + k;
                    row.eid = 'e' + n;
                    row.iframe_index = anchor ?? row.iframe_index;
                }}
                return JSON.stringify(out);
            }})()"#,
        );
        let raw = {
            let page = page.clone();
            self.run(async move { page.evaluate_stealth(script).await })
                .await?
        };
        let entries: Vec<ScriptedRef> = match raw {
            Some(serde_json::Value::String(encoded)) => serde_json::from_str(&encoded)
                .map_err(|e| BackendError::Engine(format!("snapshot decode failed: {e}")))?,
            // A non-string result means the script itself failed (an
            // exception now surfaces as Engine, but a silent null —
            // e.g. a torn-down execution context — must not masquerade
            // as "the page has no interactive elements").
            None => {
                return Err(BackendError::Engine(
                    "snapshot script produced no result (the page may have navigated mid-read; retry)"
                        .to_owned(),
                ));
            }
            _ => Vec::new(),
        };

        let url = page.url().await.ok().flatten();
        let title = page.title().await.ok().flatten();

        // Cross-origin (OOPIF) frames: match each top-level iframe
        // placeholder against the live `type: "iframe"` targets by src
        // prefix (src-equal groups match by placeholder order), then walk
        // inside the matched target — same interactive script, iframe-
        // local geometry — emitting `g<k>:e<n>` refs. Enumeration and
        // matching happen per snapshot: OOPIF targets re-create on every
        // navigation, so nothing here is cached across commands.
        let (entries, mut oopif_frames) = self.resolve_oopif_frames(tab, entries).await;

        // Mint refs, cache geometry, render the tree. Frame rows carry
        // their `f<k>:e<n>` label; iframe placeholders render as
        // non-clickable frame rows with the src.
        let mut refs = Vec::with_capacity(entries.len());
        let mut ref_entries = Vec::with_capacity(entries.len());
        let mut tree = String::new();
        tree.push_str("# ");
        tree.push_str(title.as_deref().unwrap_or("(untitled)"));
        tree.push('\n');
        tree.push_str("# ");
        tree.push_str(url.as_deref().unwrap_or(""));
        tree.push_str("\n\n");
        for entry in &entries {
            let text = entry.text.clone().filter(|t| !t.is_empty());
            let eid = match (&entry.frame, entry.clickable) {
                // Top-frame element: the script assigned `e<n>`.
                (None, true) => entry.eid.clone().unwrap_or_default(),
                // Frame element: `f<k>:<eid>`.
                (Some(frame), true) => format!("{frame}:{}", entry.eid.clone().unwrap_or_default()),
                // Iframe placeholder: no ref, an informational row. A
                // matched cross-origin frame names its `g<k>` label so
                // the agent can see the frame is walkable/clickable by
                // ref (its rows follow) or evaluate inside it.
                (_, false) => {
                    let src = text.clone().unwrap_or_else(|| "(srcdoc)".to_owned());
                    tree.push_str("- ");
                    if let Some(label) = entry.frame.as_ref().filter(|f| f.starts_with('g')) {
                        tree.push('[');
                        tree.push_str(label);
                        tree.push_str("] ");
                    }
                    tree.push_str("<iframe>");
                    if let Some(id) = entry.html_id.as_ref() {
                        tree.push_str(" #");
                        tree.push_str(id);
                    }
                    tree.push(' ');
                    tree.push_str(&src);
                    if entry.visible {
                        tree.push('\n');
                    } else {
                        tree.push_str(" [hidden]\n");
                    }
                    // A matched cross-origin frame is addressable even
                    // when its walk found no interactive elements:
                    // mint the placeholder's frame binding so `evaluate`
                    // (and click_at-style use) can resolve `g<k>`.
                    // Peek, don't consume: child refs below share the
                    // same binding.
                    let target_frame = entry.frame.as_ref().and_then(|f| {
                        f.starts_with('g')
                            .then(|| oopif_frames.get(f).cloned())
                            .flatten()
                    });
                    ref_entries.push(RefEntry {
                        id: String::new(),
                        center: (entry.x, entry.y),
                        frame: None,
                        target_frame,
                    });
                    refs.push(SnapshotRef {
                        id: String::new(),
                        tag: entry.tag.clone(),
                        text: entry.text.clone(),
                        html_id: entry.html_id.clone(),
                        value: None,
                        frame: entry.frame.clone(),
                        clickable: false,
                        x: Some(entry.x),
                        y: Some(entry.y),
                    });
                    continue;
                }
            };
            tree.push_str("- [");
            tree.push_str(&eid);
            tree.push_str("] <");
            tree.push_str(&entry.tag);
            let anchor = entry
                .html_id
                .as_ref()
                .map_or_else(String::new, |h| format!("#{h}"));
            tree.push_str(&anchor);
            tree.push_str("> ");
            tree.push_str(text.as_deref().unwrap_or(""));
            if !entry.visible {
                tree.push_str(" [hidden]");
            }
            tree.push('\n');
            ref_entries.push(RefEntry {
                id: eid.clone(),
                center: (entry.x, entry.y),
                frame: entry.frame.as_ref().map(|_| FrameAnchor {
                    iframe_index: entry.iframe_index.unwrap_or_default(),
                }),
                target_frame: entry
                    .frame
                    .as_ref()
                    .and_then(|f| f.starts_with('g').then(|| oopif_frames.remove(f)))
                    .flatten(),
            });
            refs.push(SnapshotRef {
                id: eid,
                tag: entry.tag.clone(),
                text: entry.text.clone(),
                html_id: entry.html_id.clone(),
                value: entry.value.clone(),
                frame: entry.frame.clone(),
                clickable: true,
                x: Some(entry.x),
                y: Some(entry.y),
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
        let (center, frame, target_frame) = {
            let state = self.inner.lock().await;
            let entry = state
                .refs
                .get(tab)
                .and_then(|refs| refs.iter().find(|r| r.id == reference))
                .ok_or_else(|| BackendError::StaleRef(reference.to_owned()))?;
            (
                entry.center,
                entry.frame.clone(),
                entry.target_frame.clone(),
            )
        };

        if let Some(anchor) = target_frame {
            tracing::debug!(tab, reference, "click: cross-origin frame path");
            let result = ChaserBackend::click_in_target_frame(self, &anchor, reference).await;
            self.observe_input_result(&result).await;
            return result;
        }

        if let Some(anchor) = frame {
            tracing::debug!(tab, reference, "click: frame path");
            let result = ChaserBackend::click_in_frame(self, &page, &anchor, reference).await;
            self.observe_input_result(&result).await;
            return result;
        }

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
                const els = document.querySelectorAll('{INTERACTIVE_SELECTOR}');
                const el = els[{index}];
                if (!el) return 'missing';
                el.scrollIntoView({{block: 'center'}});
                const r = el.getBoundingClientRect();
                return JSON.stringify({{x: Math.round(r.x + r.width/2), y: Math.round(r.y + r.height/2)}});
            }})()"#,
        );
        tracing::debug!(tab, reference, "click: dispatching scroll/center probe");
        let fresh = {
            let page = page.clone();
            self.run(async move { page.evaluate_stealth(scroll_script).await })
                .await?
        };
        tracing::debug!(tab, reference, ?fresh, "click: probe returned");
        let center = match fresh {
            Some(serde_json::Value::String(encoded)) => {
                tracing::debug!(tab, reference, "click: probe answer decoded as point");
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
        tracing::debug!(tab, reference, ?center, "click: dispatching click_human");
        let result = {
            let page = page.clone();
            self.run(async move { page.click_human(center.0, center.1).await })
                .await
        };
        self.observe_input_result(&result).await;
        result
    }

    async fn click_at(&self, tab: &str, x: f64, y: f64) -> Result<(), BackendError> {
        let page = self.page_for(tab).await?;
        // Clamp to the live viewport so a stale measured coordinate
        // cannot click outside the window. The read failing must never
        // fail the click: pass the coordinate through unclamped then.
        let clamped = match {
            let page = page.clone();
            self.run(async move {
                page.evaluate_stealth("({w: window.innerWidth, h: window.innerHeight})".to_owned())
                    .await
            })
            .await?
        } {
            Some(viewport) => {
                let width = viewport.get("w").and_then(serde_json::Value::as_f64);
                let height = viewport.get("h").and_then(serde_json::Value::as_f64);
                match (width, height) {
                    (Some(w), Some(h)) if w > 0.0 && h > 0.0 => (
                        x.clamp(0.0, (w - 1.0).max(0.0)),
                        y.clamp(0.0, (h - 1.0).max(0.0)),
                    ),
                    _ => (x, y),
                }
            }
            None => (x, y),
        };
        tracing::debug!(
            x = clamped.0,
            y = clamped.1,
            "click_at: dispatching click_human"
        );
        let result = {
            let page = page.clone();
            self.run(async move { page.click_human(clamped.0, clamped.1).await })
                .await
        };
        self.observe_input_result(&result).await;
        result
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
        let wrapped = Self::prepare_eval_script(script);
        self.run(async move { page.evaluate_stealth(wrapped).await })
            .await
    }

    async fn evaluate_in_frame(
        &self,
        tab: &str,
        frame: &str,
        script: &str,
    ) -> Result<Option<serde_json::Value>, BackendError> {
        // `tab` scopes the ref table; the frame label picks the target.
        let _ = tab;
        let anchor = {
            let state = self.inner.lock().await;
            state
                .refs
                .get(tab)
                .and_then(|refs| refs.iter().find_map(|r| r.target_frame.as_ref()))
                .filter(|t| t.label == frame)
                .cloned()
                .ok_or_else(|| {
                    BackendError::StaleRef(format!(
                        "{frame} (no such cross-origin frame in the latest snapshot; re-snapshot)"
                    ))
                })?
        };
        let frame_page = self.page_in_target_frame(&anchor).await?;
        let wrapped = Self::prepare_eval_script(script);
        self.run(async move { frame_page.evaluate_stealth(wrapped).await })
            .await
    }

    async fn screenshot(&self, tab: &str) -> Result<Vec<u8>, BackendError> {
        let page = self.page_for(tab).await?;
        self.run(async move { page.screenshot().await }).await
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
                        let ready = page
                            .evaluate_stealth("document.readyState".to_owned())
                            .await?;
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
                        let found = page.evaluate_stealth(script.clone()).await?;
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
        self.run(async move { page.close().await }).await?;
        self.forget_tab(tab).await;
        Ok(())
    }

    async fn is_live(&self) -> bool {
        ChaserBackend::has_live_engine(self).await
    }
}

impl<L: EngineLauncher> ChaserBackend<L> {
    /// Watchdog bookkeeping for input-class commands: a timeout is the only
    /// observable signature of a wedged Input domain, so consecutive
    /// timeouts past [`INPUT_TIMEOUT_WEDGE_LIMIT`] count as a death verdict
    /// and clear the engine (the next command lazily relaunches). Any
    /// successful input command resets the streak.
    async fn observe_input_result(&self, result: &Result<(), BackendError>) {
        let wedged = matches!(result, Err(BackendError::Timeout));
        let mut state = self.inner.lock().await;
        if wedged {
            state.input_timeouts += 1;
            tracing::warn!(
                streak = state.input_timeouts,
                limit = INPUT_TIMEOUT_WEDGE_LIMIT,
                "input command timed out"
            );
            if state.input_timeouts >= INPUT_TIMEOUT_WEDGE_LIMIT {
                tracing::warn!(
                    "input watchdog: consecutive timeouts reached; marking browser dead for recycle"
                );
                Self::clear_dead(&mut state);
            }
        }
        if result.is_ok() {
            state.input_timeouts = 0;
        }
    }

    /// Cross-origin frame resolution: matches the snapshot's top-level
    /// iframe placeholders against live `type: "iframe"` CDP targets and
    /// walks inside each matched target. Returns the (possibly extended)
    /// entries plus a map of ref id → [`FrameTarget`] for the `g<k>` rows.
    ///
    /// Matching: a placeholder whose src is a prefix of the target's URL
    /// (query strings differ; the path identifies the frame). Src-equal
    /// groups match in order — two ads from one URL bind the first
    /// placeholder to the first target, and a count mismatch degrades to
    /// leaving the extras as placeholders (a StaleRef at click time, never
    /// a wrong-frame click). Only top-level placeholders (no `f<k>` parent)
    /// participate: nested OOPIFs are rare and the parent-projected frame
    /// tree makes their target attribution ambiguous.
    ///
    /// Every failure inside a matched frame (target gone mid-walk, eval
    /// error, no result) leaves that frame's rows as plain placeholders —
    /// degraded, never fatal: the main-frame snapshot must still land.
    async fn resolve_oopif_frames(
        &self,
        tab: &str,
        mut entries: Vec<ScriptedRef>,
    ) -> (Vec<ScriptedRef>, HashMap<String, FrameTarget>) {
        // Collect top-level cross-origin placeholders in document order.
        let placeholders: Vec<(usize, String)> = entries
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                // Placeholder rows: tag iframe, not clickable, top-level
                // (`f<k>` parents are same-origin — already walked), and
                // carrying their frame-list index.
                !e.clickable && e.tag == "iframe" && e.frame.is_none() && e.iframe_index.is_some()
            })
            .map(|(i, e)| (i, e.text.clone().unwrap_or_default()))
            .collect();
        tracing::debug!(count = placeholders.len(), srcs = ?placeholders, "oopif: placeholders");
        if placeholders.is_empty() {
            return (entries, HashMap::new());
        }

        // Enumerate the live iframe targets. An enumeration failure is
        // degraded (placeholder-only snapshot), not fatal. The state
        // guard is held across the engine call: engine-handle methods
        // never lock backend state, so there is no re-entrancy, and
        // `Box<dyn EngineHandle>` cannot be cloned out of the engine.
        let targets = {
            let state = self.inner.lock().await;
            let Some(engine) = state.engine.as_ref() else {
                return (entries, HashMap::new());
            };
            match engine.handle.list_targets().await {
                Ok(t) => t,
                Err(e) => {
                    tracing::debug!(tab, error = %e, "oopif: target enumeration failed");
                    return (entries, HashMap::new());
                }
            }
        };
        let mut iframe_targets: Vec<&crate::surface::TargetInfo> =
            targets.iter().filter(|t| t.r#type == "iframe").collect();
        // Stable order: browser enumeration order is already creation
        // order; sort by url so src-equal groups bind deterministically.
        iframe_targets.sort_by(|a, b| a.url.cmp(&b.url));

        let mut matched: Vec<(usize, String, &crate::surface::TargetInfo)> = Vec::new();
        let mut available: Vec<&crate::surface::TargetInfo> = iframe_targets;
        for (entry_index, src) in placeholders {
            let bind = available.iter().position(|t| urls_share_path(&t.url, &src));
            if let Some(pos) = bind {
                let target = available.remove(pos);
                matched.push((entry_index, src, target));
            }
        }
        tracing::debug!(matched = matched.len(), targets = ?matched.iter().map(|(_,_,t)| t.url.clone()).collect::<Vec<_>>(), "oopif: matched");
        if matched.is_empty() {
            return (entries, HashMap::new());
        }

        // Walk inside each matched target and label its rows.
        let mut ref_frame: HashMap<String, FrameTarget> = HashMap::new();
        let mut appended: Vec<(usize, ScriptedRef)> = Vec::new();
        for (g_number, (entry_index, _src, target)) in matched.iter().enumerate() {
            let label = format!("g{}", g_number + 1);
            let target_id = target.target_id.clone();
            let frame_page = {
                let state = self.inner.lock().await;
                let Some(engine) = state.engine.as_ref() else {
                    continue;
                };
                let target_id =
                    chaser_oxide::cdp::browser_protocol::target::TargetId::from(target_id.clone());
                // First-touch race: chaser's handler only drains queued
                // `Target.attachedToTarget` events when polled, so a frame
                // that committed microseconds ago may have no session yet
                // (get_page → NotFound) even though the attach is imminent.
                // One short bounce lets the next poll register it.
                let mut attempt = 0;
                loop {
                    match engine.handle.get_page(&target_id).await {
                        Ok(p) => break Some(p),
                        Err(e) if attempt == 0 => {
                            attempt = 1;
                            tracing::debug!(error = %e, "oopif: get_page first attempt failed; retrying after settle");
                            tokio::time::sleep(std::time::Duration::from_millis(120)).await;
                        }
                        Err(e) => {
                            tracing::debug!(error = %e, "oopif: get_page failed");
                            break None;
                        }
                    }
                }
            };
            let Some(frame_page) = frame_page else {
                continue;
            };
            // The in-frame walk: the same selector, iframe-local geometry.
            let script = format!(
                "(() => {{ const SEL = '{INTERACTIVE_SELECTOR}'; {} }})()",
                OOPIF_WALK_TAIL
            );
            let rows = {
                let frame_page = frame_page.clone();
                match self
                    .run(async move { frame_page.evaluate_stealth(script).await })
                    .await
                {
                    Ok(Some(serde_json::Value::String(encoded))) => {
                        serde_json::from_str::<Vec<ScriptedRef>>(&encoded).ok()
                    }
                    _ => None,
                }
            };
            let Some(rows) = rows else {
                tracing::debug!(target_id, "oopif: in-frame walk failed or empty");
                continue;
            };
            // Rewrite the placeholder row to name its frame, and record
            // the binding so the click path (and evaluate's `frame`) can
            // resolve the target live. Placeholder keeps clickable=false
            // (the frame is not a button) but gains the g-label.
            if let Some(placeholder) = entries.get_mut(*entry_index) {
                placeholder.frame = Some(label.clone());
            }
            let frame_target = FrameTarget {
                label: label.clone(),
                target_id,
                iframe_index: entries
                    .get(*entry_index)
                    .and_then(|e| e.iframe_index)
                    .unwrap_or_default(),
            };
            ref_frame.insert(label.clone(), frame_target);
            // The in-frame rows: tag each with the frame label; the
            // minting loop prefixes their eid with `g<k>`. iframe_index
            // stays None — it only anchors same-origin `f<k>` clicks.
            for mut row in rows {
                row.iframe_index = None;
                appended.push((*entry_index, row));
            }
        }

        // Splice appended rows after their placeholder, in placeholder
        // order (iterate reversed so earlier indices stay valid).
        for (entry_index, mut row) in appended {
            row.frame = entries
                .get(entry_index)
                .and_then(|e| e.frame.clone())
                .or_else(|| row.frame.take());
            entries.insert(entry_index + 1, row);
        }
        (entries, ref_frame)
    }

    /// Resolves the live page handle for a `g<k>` frame target. The
    /// snapshot's target id is a hint: the target list is re-read and a
    /// target with the same id is required to still exist and still be
    /// type iframe (OOPIF targets re-create per navigation with fresh
    /// ids, so a stale hint falls through to src re-matching).
    async fn page_in_target_frame(
        &self,
        anchor: &FrameTarget,
    ) -> Result<Arc<dyn PageHandle>, BackendError> {
        // The state guard is held across the engine calls (no re-entrancy:
        // engine-handle methods never lock backend state).
        let state = self.inner.lock().await;
        let Some(engine) = state.engine.as_ref() else {
            return Err(BackendError::DeadBrowser("no live browser".to_owned()));
        };
        let targets = engine.handle.list_targets().await?;
        let live = targets.iter().find(|t| t.target_id == anchor.target_id);
        match live {
            Some(t) if t.r#type == "iframe" => {
                let tid = chaser_oxide::cdp::browser_protocol::target::TargetId::from(
                    t.target_id.clone(),
                );
                engine.handle.get_page(&tid).await
            }
            _ => Err(BackendError::StaleRef(format!(
                "{} (cross-origin frame re-created; re-snapshot)",
                anchor.label
            ))),
        }
    }

    /// The eval-mode rewrite: a bare expression evaluates as-is (its
    /// value survives); a statement-shaped script (contains `return`/
    /// `throw` outside strings) is wrapped in an IIFE, which makes the
    /// `return` legal. Shared by the top-frame and in-frame evaluate
    /// paths.
    fn prepare_eval_script(script: &str) -> String {
        // An agent-written IIFE already produces a completion value —
        // wrapping it in another arrow function discards that value
        // (the inner expression becomes a statement). Any `return` the
        // detector then finds lives inside the agent's own function, not
        // at top level. Detect the IIFE tail outside strings/comments
        // and pass such scripts through untouched.
        if script_ends_with_iife_call(script) {
            return script.to_owned();
        }
        if script_looks_like_statements(script) {
            format!("(() => {{\n{script}\n}})()")
        } else {
            script.to_owned()
        }
    }

    /// Clicks a `g<k>:e<n>` ref: resolve the iframe target's page, run
    /// the scroll/center walk inside it (iframe-local coordinates), and
    /// dispatch the trusted click on the iframe's own session — a
    /// top-frame dispatch never routes into an out-of-process frame
    /// (CDP-proven failure mode).
    async fn click_in_target_frame(
        &self,
        anchor: &FrameTarget,
        reference: &str,
    ) -> Result<(), BackendError> {
        let (_label, eid) = reference
            .split_once(':')
            .ok_or_else(|| BackendError::StaleRef(reference.to_owned()))?;
        let index = eid
            .trim_start_matches('e')
            .parse::<usize>()
            .map_err(|e| BackendError::Engine(format!("bad ref `{reference}`: {e}")))?
            .checked_sub(1)
            .ok_or_else(|| BackendError::StaleRef(reference.to_owned()))?;
        let frame_page = self.page_in_target_frame(anchor).await?;
        let script = format!(
            r#"(() => {{
                const SEL = '{INTERACTIVE_SELECTOR}';
                const els = document.querySelectorAll(SEL);
                const el = els[{index}];
                if (!el) return 'missing';
                el.scrollIntoView({{block: 'center'}});
                const r = el.getBoundingClientRect();
                return JSON.stringify({{x: Math.round(r.x + r.width/2), y: Math.round(r.y + r.height/2)}});
            }})()"#,
        );
        let fresh = {
            let frame_page = frame_page.clone();
            self.run(async move { frame_page.evaluate_stealth(script).await })
                .await?
        };
        let center = match fresh {
            Some(serde_json::Value::String(encoded)) => {
                serde_json::from_str::<serde_json::Value>(&encoded)
                    .ok()
                    .map(|p| {
                        (
                            p.get("x").and_then(serde_json::Value::as_f64),
                            p.get("y").and_then(serde_json::Value::as_f64),
                        )
                    })
                    .unwrap_or((None, None))
            }
            _ => (None, None),
        };
        let (x, y) = match center {
            (Some(x), Some(y)) => (x, y),
            _ => {
                return Err(BackendError::StaleRef(format!(
                    "{reference} (element moved inside the frame; re-snapshot)"
                )));
            }
        };
        tracing::debug!(x, y, "click_in_target_frame: dispatching on iframe session");
        let result = {
            let frame_page = frame_page.clone();
            self.run(async move { frame_page.click_human(x, y).await })
                .await
        };
        self.observe_input_result(&result).await;
        result
    }

    /// Clicks a frame ref (`f<k>:e<n>`): re-resolve the iframe element
    /// in the parent document (its viewport rect is the offset), run
    /// the scroll/center script inside the child document (same-origin
    /// access), sum the two, trusted-click at the viewport point. A
    /// frame that navigated away or went cross-origin surfaces as a
    /// stale ref — re-snapshot is the recovery.
    /// Clicks a frame ref (`f<k>:e<n>`): re-resolve the iframe element
    /// in the parent document (its viewport rect is the offset), run
    /// the scroll/center script inside the child document (same-origin
    /// access), sum the two, trusted-click at the viewport point. A
    /// frame that navigated away or went cross-origin surfaces as a
    /// stale ref — re-snapshot is the recovery.
    async fn click_in_frame(
        &self,
        page: &Arc<dyn PageHandle>,
        anchor: &FrameAnchor,
        reference: &str,
    ) -> Result<(), BackendError> {
        let (_eid, index) = reference
            .split_once(':')
            .map(|(_frame, eid)| (eid, eid.trim_start_matches('e').parse::<usize>().ok()))
            .ok_or_else(|| BackendError::StaleRef(reference.to_owned()))?;
        let in_frame_index = index
            .and_then(|n| n.checked_sub(1))
            .ok_or_else(|| BackendError::StaleRef(reference.to_owned()))?;
        let iframe_index = anchor.iframe_index;
        let script = format!(
            r#"(() => {{
                const frames = document.querySelectorAll('iframe, frame');
                const fr = frames[{iframe_index}];
                if (!fr) return 'frame missing';
                let doc = null;
                try {{ doc = fr.contentDocument; }} catch (e) {{ return 'cross-origin'; }}
                if (!doc || !doc.documentElement) return 'cross-origin';
                const frRect = fr.getBoundingClientRect();
                const els = doc.querySelectorAll('{INTERACTIVE_SELECTOR}');
                const el = els[{in_frame_index}];
                if (!el) return 'missing';
                el.scrollIntoView({{block: 'center'}});
                const r = el.getBoundingClientRect();
                return JSON.stringify({{x: Math.round(frRect.x + r.x + r.width/2), y: Math.round(frRect.y + r.y + r.height/2)}});
            }})()"#,
        );
        let fresh = {
            let page = page.clone();
            self.run(async move { page.evaluate_stealth(script).await })
                .await?
        };
        tracing::debug!(reference, ?fresh, "click_in_frame: probe returned");
        // A probe answer is either the JSON point or a plain marker
        // string ('cross-origin' / 'frame missing' / 'missing'); any
        // non-point answer is the stale path, not an engine error.
        let center = match fresh {
            Some(serde_json::Value::String(encoded)) => {
                serde_json::from_str::<serde_json::Value>(&encoded)
                    .ok()
                    .map(|parsed| {
                        (
                            parsed.get("x").and_then(serde_json::Value::as_f64),
                            parsed.get("y").and_then(serde_json::Value::as_f64),
                        )
                    })
                    .unwrap_or((None, None))
            }
            _ => (None, None),
        };
        let (x, y) = match center {
            (Some(x), Some(y)) => (x, y),
            _ => {
                // 'frame missing' / 'cross-origin' / 'missing' — all are
                // "the snapshot is stale" for the agent's purposes.
                return Err(BackendError::StaleRef(format!(
                    "{reference} (frame changed or went cross-origin; re-snapshot)"
                )));
            }
        };
        tracing::debug!(x, y, "click_in_frame: dispatching click_human");
        let result = {
            let page = page.clone();
            self.run(async move { page.click_human(x, y).await }).await
        };
        self.observe_input_result(&result).await;
        result
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
            url: String,
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

        async fn list_targets(&self) -> Result<Vec<crate::surface::TargetInfo>, BackendError> {
            let mut browser = self.browser.lock().await;
            let targets = browser.fetch_targets().await.map_err(map_engine_error)?;
            Ok(targets
                .into_iter()
                .map(|t| crate::surface::TargetInfo {
                    target_id: t.target_id.as_ref().to_owned(),
                    r#type: t.r#type.clone(),
                    url: t.url,
                })
                .collect())
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
        async fn goto(&self, url: String) -> Result<(), BackendError> {
            self.page
                .goto(&url)
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
            script: String,
        ) -> Result<Option<serde_json::Value>, BackendError> {
            self.page
                .evaluate_stealth(&script)
                .await
                .map_err(map_engine_error)
        }

        async fn click_human(&self, x: f64, y: f64) -> Result<(), BackendError> {
            tracing::debug!(x, y, "chaser click_human: start");
            let r = self.page.click_human(x, y).await;
            tracing::debug!(x, y, ok = r.is_ok(), "chaser click_human: done");
            r.map_err(map_engine_error)
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
        // Native Wayland pacing: on a Wayland compositor Chromium prefers
        // its native backend, whose window-occlusion tracker frame-throttles
        // a non-active window to one wakeup per 5s — exactly 5s per
        // Input.dispatchMouseEvent ack. Forcing the XWayland backend keeps
        // input dispatches frame-unthrottled regardless of window focus
        // (the X11 path has no native occlusion tracker to fight).
        let ozone_platform = std::env::var("WAYLAND_DISPLAY")
            .is_ok()
            .then_some(("OZONE_PLATFORM", "x11".to_owned()));
        let config = {
            let mut builder = chaser_oxide::BrowserConfig::builder()
                .user_data_dir(user_data_dir)
                .chrome_executable(executable)
                .viewport(None)
                // Headed — never headless (S1–S7 signals, focus texture).
                .with_head();
            if let Some((key, value)) = ozone_platform {
                builder = builder.env(key, value);
            }
            builder
                .build()
                .map_err(|e| BackendError::Engine(format!("browser config rejected: {e}")))?
        };

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
