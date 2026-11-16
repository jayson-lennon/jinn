//! Supervision and recycle tests: the liveness wrapper's observable
//! behavior, exercised through fake launchers/handles — no real Chrome.
//!
//! Covered: launch-on-first-command; a command-reported dead transport
//! flips the state dead and the next command recycles; a child exit (or
//! an unreadable child) under the supervision tick does the same and
//! clears the tab table; a command timeout is a command failure that
//! leaves the engine live; and every eval the seam exposes is the
//! stealth path.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "test assertions need expect/panic to state what went wrong"
)]

use std::sync::Arc;

use crate::backend::{
    BackendError, BrowserBackend, ChaserBackend, DeathSignal, EngineHandle, EngineLauncher,
    LiveEngine, PageHandle, script_looks_like_statements,
};
use tokio::sync::Mutex;

/// What a fake page's stealth eval does.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PageMode {
    /// Returns `"ok"`.
    Ok,
    /// Reports a dead transport.
    Died,
    /// Sleeps past any command timeout.
    Sleeps,
    /// Evaluates answer quickly (snapshot/viewport reads work) but the
    /// mouse dispatch hangs — a wedged Chromium input path.
    ClickSleeps,
    /// Records the script and returns `"ok"`.
    Records,
    /// Returns `None` for every script (a swallowed script exception —
    /// the pre-E1-fix shape).
    ReturnsNull,
    /// Answers snapshot-shaped scripts with the fixture's scripted
    /// payload string; other scripts return `"ok"`.
    Scripted,
}
struct FakePage {
    mode: Arc<Mutex<PageMode>>,
    seen_scripts: Arc<Mutex<Vec<String>>>,
    clicked_at: Arc<Mutex<Vec<(f64, f64)>>>,
    viewport: Arc<Mutex<Option<(f64, f64)>>>,
    scripted_payload: Arc<Mutex<Option<String>>>,
}

impl FakePage {
    fn new(
        mode: Arc<Mutex<PageMode>>,
        seen_scripts: Arc<Mutex<Vec<String>>>,
        clicked_at: Arc<Mutex<Vec<(f64, f64)>>>,
        viewport: Arc<Mutex<Option<(f64, f64)>>>,
    ) -> Self {
        Self {
            mode,
            seen_scripts,
            clicked_at,
            viewport,
            scripted_payload: Arc::new(Mutex::new(None)),
        }
    }

    fn with_payload(mut self, payload: Arc<Mutex<Option<String>>>) -> Self {
        self.scripted_payload = payload;
        self
    }
}

#[async_trait::async_trait]
impl PageHandle for FakePage {
    async fn goto(&self, _url: String) -> Result<(), BackendError> {
        Ok(())
    }

    async fn url(&self) -> Result<Option<String>, BackendError> {
        Ok(Some("https://fake.test/".to_owned()))
    }

    async fn title(&self) -> Result<Option<String>, BackendError> {
        Ok(Some("Fake".to_owned()))
    }

    async fn evaluate_stealth(
        &self,
        _script: String,
    ) -> Result<Option<serde_json::Value>, BackendError> {
        match *self.mode.lock().await {
            PageMode::Died => Err(BackendError::DeadBrowser("transport closed".to_owned())),
            PageMode::Sleeps => {
                // Outlast any command timeout; the backend's timeout is
                // what returns. Blocking sleep in a spawn_blocking-free
                // test would stall the runtime, so yield forever instead.
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
                }
            }
            PageMode::ClickSleeps => {
                if _script.contains("collectAll") {
                    let payload = self.scripted_payload.lock().await.clone();
                    return Ok(payload.map(serde_json::Value::String));
                }
                if _script.contains("innerWidth") {
                    let vp = *self.viewport.lock().await;
                    return Ok(vp.map(|(w, h)| serde_json::json!({"w": w, "h": h})));
                }
                if _script.contains("scrollIntoView") {
                    // The click path's scroll/center probe: answer with a
                    // point so the flow reaches the (hanging) dispatch.
                    return Ok(Some(serde_json::Value::String(
                        r#"{"x":12,"y":12}"#.to_owned(),
                    )));
                }
                Ok(Some(serde_json::Value::String("ok".to_owned())))
            }
            PageMode::Records => {
                self.seen_scripts.lock().await.push(_script.clone());
                Ok(Some(serde_json::Value::String("ok".to_owned())))
            }
            PageMode::ReturnsNull => Ok(None),
            PageMode::Scripted => {
                let script: String = _script;
                if script.contains("collectAll") {
                    let payload = self.scripted_payload.lock().await.clone();
                    return Ok(payload.map(serde_json::Value::String));
                }
                if script.contains("frame missing") {
                    // The frame click path's scroll/center probe: answer
                    // with the JSON.stringify'd viewport-absolute center
                    // (iframe offset + in-frame rect already summed by
                    // the script) — the double-encoded wire shape the
                    // real isolated-world eval returns.
                    return Ok(Some(serde_json::Value::String(
                        r#"{"x":248.0,"y":322.0}"#.to_owned(),
                    )));
                }
                Ok(Some(serde_json::Value::String("ok".to_owned())))
            }
            PageMode::Ok => {
                // The click_at clamp reads the viewport with a bare
                // object expression; answer it from the fake viewport.
                if _script.contains("innerWidth") {
                    let vp = *self.viewport.lock().await;
                    return Ok(vp.map(|(w, h)| serde_json::json!({"w": w, "h": h})));
                }
                if _script.contains("scrollIntoView") {
                    // A frame probe (contentDocument present) answers a
                    // marker — the stale-ref shape; a top-frame probe
                    // answers a point so the flow reaches the dispatch.
                    if _script.contains("contentDocument") {
                        return Ok(Some(serde_json::Value::String("missing".to_owned())));
                    }
                    return Ok(Some(serde_json::Value::String(
                        r#"{"x":12,"y":12}"#.to_owned(),
                    )));
                }
                Ok(Some(serde_json::Value::String("ok".to_owned())))
            }
        }
    }

    async fn click_human(&self, x: f64, y: f64) -> Result<(), BackendError> {
        // Entry record = the press dispatched. In a sleep mode the
        // dispatch then hangs past the command timeout before the exit
        // record (= the release) lands — mirroring a wedged input path
        // whose press/release pair is in flight when the awaiting
        // command gives up.
        let hung = matches!(
            *self.mode.lock().await,
            PageMode::Sleeps | PageMode::ClickSleeps
        );
        self.clicked_at.lock().await.push((x, y));
        if hung {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            self.clicked_at.lock().await.push((x, y));
        }
        Ok(())
    }

    async fn type_text(&self, _text: &str) -> Result<(), BackendError> {
        Ok(())
    }

    async fn press_key(&self, _key: &str) -> Result<(), BackendError> {
        Ok(())
    }

    async fn screenshot(&self) -> Result<Vec<u8>, BackendError> {
        Ok(vec![0x89, b'P', b'N', b'G'])
    }

    async fn close(&self) -> Result<(), BackendError> {
        Ok(())
    }
}

struct FakeEngine {
    child_exited: Arc<Mutex<bool>>,
    try_wait_io_error: Arc<Mutex<bool>>,
    page_mode: Arc<Mutex<PageMode>>,
    seen_scripts: Arc<Mutex<Vec<String>>>,
    clicked_at: Arc<Mutex<Vec<(f64, f64)>>>,
    viewport: Arc<Mutex<Option<(f64, f64)>>>,
    scripted_payload: Arc<Mutex<Option<String>>>,
}

#[async_trait::async_trait]
impl EngineHandle for FakeEngine {
    async fn new_page(
        &self,
        _url: String,
    ) -> Result<
        (
            chaser_oxide::cdp::browser_protocol::target::TargetId,
            Arc<dyn PageHandle>,
        ),
        BackendError,
    > {
        // A distinct target per page keeps the backend's table honest;
        // the exact string is opaque to the seam.
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let target =
            chaser_oxide::cdp::browser_protocol::target::TargetId::new(format!("fake-target-{n}"));
        let page = FakePage::new(
            self.page_mode.clone(),
            self.seen_scripts.clone(),
            self.clicked_at.clone(),
            self.viewport.clone(),
        )
        .with_payload(self.scripted_payload.clone());
        Ok((target, Arc::new(page)))
    }

    async fn get_page(
        &self,
        _target: &chaser_oxide::cdp::browser_protocol::target::TargetId,
    ) -> Result<Arc<dyn PageHandle>, BackendError> {
        let page = FakePage::new(
            self.page_mode.clone(),
            self.seen_scripts.clone(),
            self.clicked_at.clone(),
            self.viewport.clone(),
        )
        .with_payload(self.scripted_payload.clone());
        Ok(Arc::new(page))
    }

    async fn list_targets(&self) -> Result<Vec<crate::TargetInfo>, BackendError> {
        // The fake reports no iframe targets: cross-origin resolution
        // degrades to placeholder rows, which is exactly what the
        // snapshot tests assert against.
        Ok(Vec::new())
    }

    async fn try_wait(&self) -> std::io::Result<Option<String>> {
        if *self.try_wait_io_error.lock().await {
            return Err(std::io::Error::other("child state unreadable"));
        }
        Ok((*self.child_exited.lock().await).then(|| "exit status: 1".to_owned()))
    }

    async fn shutdown(&self) -> Result<(), BackendError> {
        Ok(())
    }
}

#[derive(Clone)]
struct Fixture {
    child_exited: Arc<Mutex<bool>>,
    try_wait_io_error: Arc<Mutex<bool>>,
    page_mode: Arc<Mutex<PageMode>>,
    seen_scripts: Arc<Mutex<Vec<String>>>,
    clicked_at: Arc<Mutex<Vec<(f64, f64)>>>,
    viewport: Arc<Mutex<Option<(f64, f64)>>>,
    scripted_payload: Arc<Mutex<Option<String>>>,
    launches: Arc<std::sync::atomic::AtomicU32>,
}

impl Fixture {
    fn healthy() -> Self {
        Self::with_mode(PageMode::Ok)
    }

    fn with_mode(mode: PageMode) -> Self {
        Self {
            child_exited: Arc::new(Mutex::new(false)),
            try_wait_io_error: Arc::new(Mutex::new(false)),
            page_mode: Arc::new(Mutex::new(mode)),
            seen_scripts: Arc::new(Mutex::new(Vec::new())),
            clicked_at: Arc::new(Mutex::new(Vec::new())),
            viewport: Arc::new(Mutex::new(None)),
            scripted_payload: Arc::new(Mutex::new(None)),
            launches: Arc::new(std::sync::atomic::AtomicU32::new(0)),
        }
    }

    fn launcher(&self) -> Arc<FixtureLauncher> {
        Arc::new(FixtureLauncher {
            inner: self.clone(),
        })
    }

    fn backend(&self) -> ChaserBackend<FixtureLauncher> {
        ChaserBackend::new(
            self.launcher(),
            std::path::PathBuf::from("fake-chrome"),
            std::path::PathBuf::from("/tmp/fake-profile"),
        )
    }
}

/// The fake launcher: counts launches and hands out engines wired to the
/// fixture's shared knobs.
struct FixtureLauncher {
    inner: Fixture,
}

#[async_trait::async_trait]
impl EngineLauncher for FixtureLauncher {
    async fn launch(
        &self,
        _executable: &std::path::Path,
        _user_data_dir: &std::path::Path,
    ) -> Result<LiveEngine, BackendError> {
        self.inner
            .launches
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // The fake's stream never dies; tests flip death via the knobs
        // (page mode / child exit) instead of the channel.
        let (_death_tx, death_rx) = tokio::sync::watch::channel(None::<String>);
        Ok(LiveEngine {
            handle: Box::new(FakeEngine {
                child_exited: self.inner.child_exited.clone(),
                try_wait_io_error: self.inner.try_wait_io_error.clone(),
                page_mode: self.inner.page_mode.clone(),
                seen_scripts: self.inner.seen_scripts.clone(),
                clicked_at: self.inner.clicked_at.clone(),
                viewport: self.inner.viewport.clone(),
                scripted_payload: self.inner.scripted_payload.clone(),
            }),
            death: death_rx,
        })
    }
}

#[rstest::rstest]
#[tokio::test]
async fn first_command_launches_the_engine_and_mints_tab_ids_in_order() {
    // Given a backend over a healthy fake.
    let fixture = Fixture::healthy();
    let backend = fixture.backend();

    // When opening two tabs.
    let first = backend
        .open_tab(Some("https://fake.test/"))
        .await
        .expect("open");
    let second = backend.open_tab(None).await.expect("open second");

    // Then the engine launched once and ids minted in order.
    assert_eq!(
        fixture.launches.load(std::sync::atomic::Ordering::Relaxed),
        1
    );
    assert_eq!(first.tab, "t1");
    assert_eq!(second.tab, "t2");
}

#[rstest::rstest]
#[tokio::test]
async fn a_dead_transport_marks_dead_and_the_next_command_recycles() {
    // Given a healthy engine with one open tab.
    let fixture = Fixture::healthy();
    let backend = fixture.backend();
    let tab = backend
        .open_tab(Some("https://fake.test/"))
        .await
        .expect("open");

    // When the page mode flips to died and a command runs.
    *fixture.page_mode.lock().await = PageMode::Died;
    let result = backend.evaluate(&tab.tab, "1").await;

    // Then the command reports the dead-browser verdict and the state
    // cleared…
    assert!(matches!(result, Err(BackendError::DeadBrowser(_))));
    assert!(!backend.is_live().await);

    // …and the next command relaunches transparently (second launch,
    // fresh tab table — the old id is gone).
    let next = backend
        .open_tab(Some("https://fake.test/"))
        .await
        .expect("reopen");
    assert_eq!(
        fixture.launches.load(std::sync::atomic::Ordering::Relaxed),
        2,
        "the recycle relaunches"
    );
    assert_eq!(next.tab, "t2", "minted ids are never reused");
}

#[rstest::rstest]
#[tokio::test]
async fn tabs_from_before_a_recycle_report_unknown_tab() {
    // Given an engine that died and was recycled by a later command.
    let fixture = Fixture::healthy();
    let backend = fixture.backend();
    backend
        .open_tab(Some("https://fake.test/"))
        .await
        .expect("open");
    *fixture.page_mode.lock().await = PageMode::Died;
    let _ = backend.evaluate("t1", "1").await; // the verdict

    // When the recycle happens (next command) and the old tab is named.
    let next = backend.open_tab(None).await.expect("recycle opens");
    let stale = backend.navigate("t1", "https://fake.test/2").await;

    // Then the pre-recycle tab id is unknown — the designed recovery is
    // list_tabs, not resurrection.
    assert!(matches!(stale, Err(BackendError::UnknownTab(_))));
    assert_eq!(next.tab, "t2");
}

#[rstest::rstest]
#[tokio::test]
async fn child_exit_under_supervision_clears_tabs_and_the_next_open_recycles() {
    // Given a backend with one open tab whose child has exited.
    let fixture = Fixture::healthy();
    let backend = fixture.backend();
    backend
        .open_tab(Some("https://fake.test/"))
        .await
        .expect("open");
    *fixture.child_exited.lock().await = true;

    // When supervision polls.
    let signal = backend.poll_supervision().await;

    // Then a child-exit death signal comes back and the state is dead.
    assert!(matches!(signal, Some(DeathSignal::Exited(_))));
    assert!(!backend.is_live().await);

    // And the next open recycles.
    let tab = backend
        .open_tab(Some("https://fake.test/"))
        .await
        .expect("reopen");
    assert_eq!(tab.tab, "t2");
    assert_eq!(
        fixture.launches.load(std::sync::atomic::Ordering::Relaxed),
        2
    );
}

#[rstest::rstest]
#[tokio::test]
async fn an_unreadable_child_state_is_a_death_verdict_under_supervision() {
    // Given a backend whose engine cannot report child state.
    let fixture = Fixture::healthy();
    let backend = fixture.backend();
    backend
        .open_tab(Some("https://fake.test/"))
        .await
        .expect("open");
    *fixture.try_wait_io_error.lock().await = true;

    // When supervision polls.
    let signal = backend.poll_supervision().await;

    // Then the io failure is a death verdict, not a swallowed error.
    assert!(matches!(signal, Some(DeathSignal::Exited(_))));
    assert!(!backend.is_live().await);
}

#[rstest::rstest]
#[tokio::test]
async fn supervision_on_a_live_engine_reports_nothing() {
    // Given a healthy backend with an open tab.
    let fixture = Fixture::healthy();
    let backend = fixture.backend();
    backend
        .open_tab(Some("https://fake.test/"))
        .await
        .expect("open");

    // When supervision polls.
    let signal = backend.poll_supervision().await;

    // Then nothing is reported and the engine stays live.
    assert!(signal.is_none());
    assert!(backend.is_live().await);
}

#[rstest::rstest]
#[tokio::test]
async fn a_command_timeout_is_not_a_death_verdict() {
    // Given a backend whose page eval sleeps past a short command
    // timeout.
    let fixture = Fixture::with_mode(PageMode::Sleeps);
    let backend = fixture
        .backend()
        .with_command_timeout(std::time::Duration::from_millis(150));
    let tab = backend
        .open_tab(Some("https://fake.test/"))
        .await
        .expect("open");

    // When a command runs (the page sleeps past the timeout).
    let result = backend.evaluate(&tab.tab, "1").await;

    // Then the command times out but the engine stays live — a timeout
    // is a command failure, never a death verdict.
    assert!(matches!(result, Err(BackendError::Timeout)));
    assert!(backend.is_live().await);
    assert_eq!(
        fixture.launches.load(std::sync::atomic::Ordering::Relaxed),
        1
    );
}

#[rstest::rstest]
#[tokio::test]
async fn evaluate_is_the_only_eval_and_it_rides_the_stealth_path() {
    // Given a backend whose pages record every script they are handed.
    let fixture = Fixture::with_mode(PageMode::Records);
    let backend = fixture.backend();
    let tab = backend
        .open_tab(Some("https://fake.test/"))
        .await
        .expect("open");

    // When evaluating a script.
    backend
        .evaluate(&tab.tab, "navigator.userAgent")
        .await
        .expect("eval");

    // Then exactly one script reached the page — the one given, through
    // the stealth entry. (The seam structurally has no other eval: the
    // PageHandle trait exposes only evaluate_stealth; this pins the
    // count so a second path cannot grow silently.)
    let seen = fixture.seen_scripts.lock().await;
    assert_eq!(seen.len(), 1);
    assert_eq!(
        seen.first().map(String::as_str),
        Some("navigator.userAgent")
    );
}

#[rstest::rstest]
#[tokio::test]
async fn a_return_script_gets_wrapped_in_an_iife() {
    // Recorded failure mode: `return document.title` is a SyntaxError
    // in the expression eval mode, and it surfaced as a silent null —
    // an agent burned six calls before discovering the expression form.
    // The backend now wraps statement-shaped scripts in an IIFE.
    //
    // Given a backend whose pages record scripts.
    let fixture = Fixture::with_mode(PageMode::Records);
    let backend = fixture.backend();
    let tab = backend
        .open_tab(Some("https://fake.test/"))
        .await
        .expect("open");

    // When evaluating the documented `return` shape…
    backend
        .evaluate(&tab.tab, "return document.title")
        .await
        .expect("eval");

    // …and a bare expression.
    backend
        .evaluate(&tab.tab, "document.title")
        .await
        .expect("eval");

    // Then the return-script arrived wrapped (making its `return`
    // legal) while the bare expression arrived untouched (so its value
    // survives the function boundary).
    let seen = fixture.seen_scripts.lock().await;
    assert_eq!(seen.len(), 2, "one script per call");
    assert_eq!(
        seen.first().map(String::as_str),
        Some("(() => {\nreturn document.title\n})()"),
        "return-scripts are IIFE-wrapped"
    );
    assert_eq!(
        seen.get(1).map(String::as_str),
        Some("document.title"),
        "bare expressions pass through unwrapped"
    );
}

#[rstest::rstest]
#[test]
fn the_statement_detector_ignores_return_inside_strings_and_comments() {
    // Given scripts whose only "return" appears inside a string literal
    // or a comment (and one that genuinely returns).
    // Then only the genuine one is detected as statement-shaped.
    assert!(script_looks_like_statements("return document.title"));
    assert!(script_looks_like_statements("const x = 1; return x;"));
    assert!(script_looks_like_statements("throw new Error('boom')"));
    assert!(!script_looks_like_statements(
        r#"document.title.includes("return")"#
    ));
    assert!(!script_looks_like_statements(
        "// return nothing here\ndocument.title"
    ));
    assert!(!script_looks_like_statements("document.title"));
}

#[rstest::rstest]
#[tokio::test]
async fn a_snapshot_whose_script_yields_no_result_is_an_error_not_an_empty_page() {
    // Recorded failure mode: a null script result mapped to `Vec::new()`
    // and the tool reported an empty tree — indistinguishable from an
    // empty page, so the agent never suspected the tool. Now the null
    // result is an engine error.
    //
    // Given a backend whose page returns null for every script.
    let fixture = Fixture::with_mode(PageMode::ReturnsNull);
    let backend = fixture.backend();
    let tab = backend
        .open_tab(Some("https://fake.test/"))
        .await
        .expect("open");

    // When snapshotting.
    let result = backend.snapshot(&tab.tab).await;

    // Then the snapshot fails naming the cause instead of returning an
    // empty page.
    let Err(BackendError::Engine(message)) = result else {
        panic!("a null snapshot result must be an error, got {result:?}");
    };
    assert!(
        message.contains("produced no result"),
        "message names the failure: {message}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn a_launch_failure_surfaces_as_an_engine_error() {
    // Given a launcher that always fails.
    let backend = ChaserBackend::new(
        Arc::new(FailingLauncher),
        std::path::PathBuf::from("fake-chrome"),
        std::path::PathBuf::from("/tmp/fake-profile"),
    );

    // When a command runs.
    let result = backend.open_tab(Some("https://fake.test/")).await;

    // Then the launch failure surfaces as an engine error naming it.
    let Err(BackendError::Engine(message)) = result else {
        panic!("expected an engine error, got {result:?}");
    };
    assert!(message.contains("no such binary"));
}

/// A launcher whose every launch fails.
struct FailingLauncher;

#[async_trait::async_trait]
impl EngineLauncher for FailingLauncher {
    async fn launch(
        &self,
        _executable: &std::path::Path,
        _user_data_dir: &std::path::Path,
    ) -> Result<LiveEngine, BackendError> {
        Err(BackendError::Engine("no such binary".to_owned()))
    }
}

#[rstest::rstest]
#[tokio::test]
async fn click_at_clamps_coordinates_to_the_live_viewport() {
    // Given a healthy backend whose fake page reports a 800x600 viewport.
    let fixture = Fixture::healthy();
    let backend = fixture.backend();
    let tab = backend
        .open_tab(Some("https://fake.test/"))
        .await
        .expect("open");
    *fixture.viewport.lock().await = Some((800.0, 600.0));

    // When clicking far outside the viewport.
    backend
        .click_at(&tab.tab, 5000.0, -40.0)
        .await
        .expect("click_at");

    // Then the dispatched click was clamped inside the viewport.
    let clicks = fixture.clicked_at.lock().await;
    assert_eq!(clicks.as_slice(), &[(799.0, 0.0)]);
}

#[rstest::rstest]
#[tokio::test]
async fn click_at_passes_through_when_the_viewport_read_fails() {
    // Given a healthy backend whose fake page answers no viewport read.
    let fixture = Fixture::healthy();
    let backend = fixture.backend();
    let tab = backend
        .open_tab(Some("https://fake.test/"))
        .await
        .expect("open");

    // When clicking at an arbitrary coordinate.
    backend
        .click_at(&tab.tab, 123.5, 88.25)
        .await
        .expect("click_at");

    // Then the coordinate rode the trusted click path unchanged.
    let clicks = fixture.clicked_at.lock().await;
    assert_eq!(clicks.as_slice(), &[(123.5, 88.25)]);
}

#[rstest::rstest]
#[tokio::test]
async fn snapshot_parses_frame_refs_and_placeholder_rows() {
    // Given a fake page whose snapshot answer is a scripted payload:
    // a top-frame element, a cross-origin iframe placeholder, and one
    // element inside a same-origin frame (labelled f1 by the script).
    let scripted = Fixture::with_mode(PageMode::Scripted);
    *scripted.scripted_payload.lock().await = Some(
        r#"[{"tag":"a","id":null,"text":"Top link","value":null,"frame":null,"eid":"e1","x":10,"y":10,"visible":true,"clickable":true},
{"tag":"iframe","id":"sp_banner","text":"https://cdn.example/banner","value":null,"frame":null,"eid":null,"x":200,"y":300,"visible":true,"clickable":false},
{"tag":"button","id":null,"text":"Close","value":null,"frame":"f1","eid":"e1","x":250,"y":320,"visible":true,"clickable":true}]"#.to_owned(),
    );
    let backend = scripted.backend();
    let tab = backend
        .open_tab(Some("https://fake.test/"))
        .await
        .expect("open");

    // When snapshotting.
    let snap = backend.snapshot(&tab.tab).await.expect("snapshot");

    // Then the top element keeps an e-ref, the iframe is a
    // non-clickable placeholder, and the frame element is f-prefixed.
    let ids: Vec<&str> = snap.refs.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, vec!["e1", "", "f1:e1"]);
    let placeholder = snap.refs.get(1).expect("iframe row");
    assert!(!placeholder.clickable);
    assert_eq!(placeholder.tag, "iframe");
    let framed = snap.refs.get(2).expect("frame element row");
    assert!(framed.id.starts_with("f1:"));
    assert!(snap.tree.contains("<iframe>"));
    assert!(snap.tree.contains("f1:e1"));
}

#[rstest::rstest]
#[tokio::test]
async fn a_frame_ref_click_resolves_through_the_frame_and_clicks_the_summed_point() {
    // Given a backend whose snapshot reported an element inside a
    // same-origin iframe (ref f1:e1) and whose frame probe answers the
    // viewport-absolute center.
    let scripted = Fixture::with_mode(PageMode::Scripted);
    *scripted.scripted_payload.lock().await = Some(
        r#"[{"tag":"button","id":null,"text":"Close","value":null,"frame":"f1","eid":"e1","x":250,"y":320,"visible":true,"clickable":true}]"#.to_owned(),
    );
    let backend = scripted.backend();
    let tab = backend
        .open_tab(Some("https://fake.test/"))
        .await
        .expect("open");
    backend.snapshot(&tab.tab).await.expect("snapshot");

    // When clicking the frame ref.
    backend.click(&tab.tab, "f1:e1").await.expect("frame click");

    // Then the trusted click landed at the summed viewport point.
    let clicks = scripted.clicked_at.lock().await;
    assert_eq!(clicks.as_slice(), &[(248.0, 322.0)]);
}

#[rstest::rstest]
#[tokio::test]
async fn a_frame_ref_click_reports_stale_when_the_frame_answer_is_not_a_point() {
    // Given a backend whose frame probe answers the 'cross-origin'
    // marker (the frame went cross-origin after the snapshot).
    let scripted = Fixture::with_mode(PageMode::Scripted);
    *scripted.scripted_payload.lock().await = Some(
        r#"[{"tag":"button","id":null,"text":"Close","value":null,"frame":"f1","eid":"e1","x":250,"y":320,"visible":true,"clickable":true}]"#.to_owned(),
    );
    let backend = scripted.backend();
    let tab = backend
        .open_tab(Some("https://fake.test/"))
        .await
        .expect("open");
    backend.snapshot(&tab.tab).await.expect("snapshot");
    // Flip the probe's answer: the fixture's Ok-mode viewport handling
    // is bypassed by returning a plain string that is not a point.
    // The Scripted mode's frame-probe arm answers a point; emulate the
    // stale case by pointing the payload at a marker the probe arm
    // does not match — here, by switching the mode to plain Ok, whose
    // answer ("ok") is not a point and not 'missing'.
    *scripted.page_mode.lock().await = PageMode::Ok;

    // When clicking the frame ref.
    let result = backend.click(&tab.tab, "f1:e1").await;

    // Then the click reports a stale ref naming the re-snapshot path.
    let Err(BackendError::StaleRef(message)) = result else {
        panic!("expected a stale ref, got {result:?}");
    };
    assert!(message.contains("re-snapshot"), "got: {message}");
}

#[rstest::rstest]
#[tokio::test]
async fn shadow_dom_rows_are_regular_refs_in_their_frame() {
    // Given a snapshot whose script surfaced a button inside a shadow
    // root (same frame as the host — the script walks roots in-frame).
    let scripted = Fixture::with_mode(PageMode::Scripted);
    *scripted.scripted_payload.lock().await = Some(
        r#"[{"tag":"a","id":null,"text":"Host link","value":null,"frame":null,"eid":"e1","x":5,"y":5,"visible":true,"clickable":true},
{"tag":"button","id":null,"text":"Shadow button","value":null,"frame":null,"eid":"e2","x":8,"y":8,"visible":true,"clickable":true}]"#.to_owned(),
    );
    let backend = scripted.backend();
    let tab = backend
        .open_tab(Some("https://fake.test/"))
        .await
        .expect("open");

    // When snapshotting.
    let snap = backend.snapshot(&tab.tab).await.expect("snapshot");

    // Then both rows are ordinary clickable top-frame refs — shadow
    // content needs no special ref notation, only the script walk.
    let ids: Vec<&str> = snap.refs.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, vec!["e1", "e2"]);
    assert!(snap.refs.iter().all(|r| r.clickable));
    assert!(snap.tree.contains("Shadow button"));
}

#[rstest::rstest]
#[tokio::test]
async fn placeholder_rows_carry_the_rect_center_for_click_at() {
    // Given a snapshot with a cross-origin iframe placeholder (the
    // recorded Guardian banner shape).
    let scripted = Fixture::with_mode(PageMode::Scripted);
    *scripted.scripted_payload.lock().await = Some(
        r#"[{"tag":"iframe","id":"sp_message_iframe_8","text":"https://cdn-sp.example/messaging","value":null,"frame":null,"eid":null,"x":1912,"y":1908,"visible":true,"clickable":false}]"#.to_owned(),
    );
    let backend = scripted.backend();
    let tab = backend
        .open_tab(Some("https://fake.test/"))
        .await
        .expect("open");

    // When snapshotting.
    let snap = backend.snapshot(&tab.tab).await.expect("snapshot");

    // Then the placeholder carries the viewport-absolute center the
    // agent feeds click_at, and no ref id (it is not clickable).
    let row = snap.refs.first().expect("placeholder row");
    assert_eq!(row.id, "");
    assert!(!row.clickable);
    assert_eq!(row.x, Some(1912.0));
    assert_eq!(row.y, Some(1908.0));
    assert_eq!(row.html_id.as_deref(), Some("sp_message_iframe_8"));
    // And the tree names the frame for the agent's frame discovery.
    assert!(snap.tree.contains("sp_message_iframe_8"));
}

#[rstest::rstest]
#[tokio::test]
async fn a_timed_out_click_still_releases_the_press() {
    // Given a backend whose snapshot works but whose mouse dispatch
    // hangs past a short command timeout — a wedged Chromium input path.
    let fixture = Fixture::with_mode(PageMode::ClickSleeps);
    fixture.scripted_payload.lock().await.replace(
        r#"[{"eid":"e1","tag":"button","text":"b","x":12,"y":12,"visible":true}]"#.to_owned(),
    );
    let backend = fixture
        .backend()
        .with_command_timeout(std::time::Duration::from_millis(150));
    let tab = backend
        .open_tab(Some("https://fake.test/"))
        .await
        .expect("open");
    backend.snapshot(&tab.tab).await.expect("snapshot");

    // When a click times out.
    let result = backend.click(&tab.tab, "e1").await;
    assert!(matches!(result, Err(BackendError::Timeout)));

    // Then the dispatch task was NOT dropped mid-sequence: the spawned
    // click completes in the background and the fake records the click
    // despite the awaiting command having timed out. (The real dispatch
    // always runs press+release to completion; dropping the future used
    // to strand the press.)
    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
    let clicks = fixture.clicked_at.lock().await;
    assert_eq!(
        clicks.len(),
        2,
        "the dispatch must run to completion (press AND release records) even though the command timed out"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn two_consecutive_input_timeouts_recycle_the_engine() {
    // Given a backend whose mouse dispatch hangs past the command
    // timeout while everything else is healthy.
    let fixture = Fixture::with_mode(PageMode::ClickSleeps);
    fixture.scripted_payload.lock().await.replace(
        r#"[{"eid":"e1","tag":"button","text":"b","x":12,"y":12,"visible":true}]"#.to_owned(),
    );
    let backend = fixture
        .backend()
        .with_command_timeout(std::time::Duration::from_millis(150));
    let tab = backend
        .open_tab(Some("https://fake.test/"))
        .await
        .expect("open");
    backend.snapshot(&tab.tab).await.expect("snapshot");

    // When two consecutive input commands time out.
    assert!(matches!(
        backend.click(&tab.tab, "e1").await,
        Err(BackendError::Timeout)
    ));
    assert!(
        backend.is_live().await,
        "first timeout alone is not a verdict"
    );
    assert!(
        matches!(
            backend.click(&tab.tab, "e1").await,
            Err(BackendError::Timeout)
        ),
        "second click reports timeout"
    );

    // Then the watchdog marks the engine dead for recycle.
    assert!(
        !backend.is_live().await,
        "reaching the wedge limit is a death verdict"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn an_input_success_resets_the_watchdog_streak() {
    // Given a backend whose first click hangs but whose later clicks
    // are healthy.
    let fixture = Fixture::with_mode(PageMode::ClickSleeps);
    fixture.scripted_payload.lock().await.replace(
        r#"[{"eid":"e1","tag":"button","text":"b","x":12,"y":12,"visible":true}]"#.to_owned(),
    );
    let backend = fixture
        .backend()
        .with_command_timeout(std::time::Duration::from_millis(150));
    let tab = backend
        .open_tab(Some("https://fake.test/"))
        .await
        .expect("open");
    backend.snapshot(&tab.tab).await.expect("snapshot");

    // When one input command times out, the mode flips healthy, and a
    // click succeeds.
    assert!(matches!(
        backend.click(&tab.tab, "e1").await,
        Err(BackendError::Timeout)
    ));
    *fixture.page_mode.lock().await = PageMode::Ok;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    backend.click(&tab.tab, "e1").await.expect("healthy click");

    // Then a subsequent timeout does NOT recycle on the first strike —
    // only consecutive timeouts reach the wedge limit.
    *fixture.page_mode.lock().await = PageMode::ClickSleeps;
    assert!(matches!(
        backend.click(&tab.tab, "e1").await,
        Err(BackendError::Timeout)
    ));
    assert!(
        backend.is_live().await,
        "a reset streak must not recycle on one timeout"
    );
}

#[rstest::rstest]
#[test]
fn urls_share_path_matches_by_path_and_ignores_query() {
    // Given a target URL with a mutated query string against a
    // placeholder src (the Sourcepoint shape: query mutates per load).
    let target = "https://cdn.privacy-mgmt.com/us_pm/index.html?hasCsp=true&uuid=1";
    let src = "https://cdn.privacy-mgmt.com/us_pm/index.html";

    // Then the URLs share a path (prefix, query ignored).
    assert!(crate::backend::urls_share_path(target, src));
    // And a different path does not match.
    assert!(!crate::backend::urls_share_path(
        "https://other.example/widget",
        src
    ));
    // And an empty srcdoc placeholder never matches anything.
    assert!(!crate::backend::urls_share_path(target, ""));
}

#[rstest::rstest]
#[test]
fn a_script_with_return_wraps_but_a_bare_expression_does_not() {
    // Given the two documented eval shapes.
    let statement = "return document.title";
    let expression = "document.title";

    // Then the statement shape wraps (its `return` must become legal)
    // and the expression shape passes through untouched (an IIFE would
    // swallow its value).
    assert!(crate::backend::script_looks_like_statements(statement));
    assert!(!crate::backend::script_looks_like_statements(expression));
}

#[rstest::rstest]
#[test]
fn the_iife_tail_detector_recognizes_iife_and_rejects_plain_statements() {
    // Given the shapes agents actually send.
    let iife_block = "(() => { return 1 + 1; })()";
    let iife_with_string = "(() => { const s = \"})()\"; return s.length; })()";
    let statements = "const a = 1;\nreturn a";

    // Then IIFE tails are detected...
    assert!(
        crate::backend::script_ends_with_iife_call(iife_block),
        "must detect plain block-body IIFE"
    );
    assert!(
        crate::backend::script_ends_with_iife_call(iife_with_string),
        "must detect IIFE containing a string with the tail inside"
    );
    // ...and plain statement scripts are not.
    assert!(!crate::backend::script_ends_with_iife_call(statements));
}
