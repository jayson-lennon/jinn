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
    /// Records the script and returns `"ok"`.
    Records,
    /// Returns `None` for every script (a swallowed script exception —
    /// the pre-E1-fix shape).
    ReturnsNull,
}

struct FakePage {
    mode: Arc<Mutex<PageMode>>,
    seen_scripts: Arc<Mutex<Vec<String>>>,
}

impl FakePage {
    fn new(mode: Arc<Mutex<PageMode>>, seen_scripts: Arc<Mutex<Vec<String>>>) -> Self {
        Self { mode, seen_scripts }
    }
}

#[async_trait::async_trait]
impl PageHandle for FakePage {
    async fn goto(&self, _url: &str) -> Result<(), BackendError> {
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
        script: &str,
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
            PageMode::Records => {
                self.seen_scripts.lock().await.push(script.to_owned());
                Ok(Some(serde_json::Value::String("ok".to_owned())))
            }
            PageMode::ReturnsNull => Ok(None),
            PageMode::Ok => Ok(Some(serde_json::Value::String("ok".to_owned()))),
        }
    }

    async fn click_human(&self, _x: f64, _y: f64) -> Result<(), BackendError> {
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
}

#[async_trait::async_trait]
impl EngineHandle for FakeEngine {
    async fn new_page(
        &self,
        _url: &str,
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
        let page = FakePage::new(self.page_mode.clone(), self.seen_scripts.clone());
        Ok((target, Arc::new(page)))
    }

    async fn get_page(
        &self,
        _target: &chaser_oxide::cdp::browser_protocol::target::TargetId,
    ) -> Result<Arc<dyn PageHandle>, BackendError> {
        let page = FakePage::new(self.page_mode.clone(), self.seen_scripts.clone());
        Ok(Arc::new(page))
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
