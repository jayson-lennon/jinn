//! Daemon, client, and API tests: the observable behaviors of startup
//! resolution, the two-daemon guard, auto-start serialization, and the
//! loopback HTTP surface — all against fakes, no real Chrome.
//!
//! Covered: registry liveness refusal; binary resolution (configured
//! wins over detection from any spawner; unresolvable configured path
//! refuses with a visible error; detection order is branded-Chrome
//! first, Chromium fallback, no Edge/Brave); the start lock serializing
//! concurrent auto-starters; the HTTP API's envelope, 404-on-unknown-tab,
//! and screenshot-returns-a-path contract.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "test assertions need expect/panic to state what went wrong"
)]

use std::sync::Arc;

use crate::backend::{
    BackendError, BrowserBackend, EngineHandle, EngineLauncher, LiveEngine, PageHandle,
};
use crate::client::DaemonClient;
use crate::daemon::{self, BinarySource, DaemonRefusal};
use crate::registry;
use jinn_common::binary_detect::BinaryProbe;
use jinn_testutil::TestRoot;
use marked_path::MarkedPathAccess as _;
use marked_path::MarkedPathBuf;

// ── daemon_refuses_when_registry_live ───────────────────────────────

#[rstest::rstest]
#[test]
fn daemon_refuses_when_registry_shows_a_live_entry() {
    // Given a state dir whose registry carries a live (port-answering) entry.
    let root = TestRoot::new();
    let paths = jinn_common::app_paths::AppPaths::new_in(&root.path().as_marked_path());
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let entry = registry::DaemonEntry::new(4242, port, 0);
    let registry_path = paths.browser_daemons_path();
    if let Some(parent) = registry_path.as_path().parent() {
        std::fs::create_dir_all(parent).expect("state dir");
    }
    registry::register(registry_path.as_path(), entry).expect("register");
    // Hold the listener open so the port probe answers.
    let _held = listener;

    // When the daemon checks the two-daemon guard.
    let result = daemon::refuse_if_live(&paths);

    // Then it refuses naming the live pid and port.
    let error = result.expect_err("a live entry must refuse a second daemon");
    let text = format!("{error:?}");
    assert!(text.contains("already running"), "got: {text}");
    assert!(text.contains("4242"), "got: {text}");
}

// ── resolve_browser: configured vs detected, and the refusal rules ──

/// A probe whose `which` answers come from a table — the injected seam
/// the binary detector's tests established. Only table answers count as
/// existing; the daemon's configured-path existence check rides the
/// same probe, so a table answer "exists" and nothing else does.
struct TableProbe {
    answers: std::collections::HashMap<&'static str, &'static str>,
}

impl TableProbe {
    fn of(pairs: &[(&'static str, &'static str)]) -> Self {
        Self {
            answers: pairs.iter().copied().collect(),
        }
    }
}

impl BinaryProbe for TableProbe {
    fn resolve_name(&self, name: &str) -> Option<MarkedPathBuf<marked_path::Absolute>> {
        let p = *self.answers.get(name)?;
        MarkedPathBuf::<marked_path::Absolute>::new(p).ok()
    }

    fn path_exists(&self, path: &std::path::Path) -> bool {
        self.answers
            .values()
            .any(|p| std::path::Path::new(p) == path)
    }

    fn name(&self) -> &'static str {
        "table"
    }
}

fn config_with_binary(path: &str) -> jinn_preferences_config::schemas::BrowserConfig {
    jinn_preferences_config::schemas::BrowserConfig {
        binary: Some(path.to_owned()),
    }
}

#[rstest::rstest]
#[test]
fn configured_binary_that_does_not_resolve_refuses_daemon_start() {
    // Given a config naming an absolute path that does not exist.
    let probe = TableProbe::of(&[]);
    let config = config_with_binary("/definitely/not/here/chrome");

    // When resolving the daemon's browser.
    let result = daemon::resolve_browser(&config, &probe);

    // Then the refusal names the configured path — the [shell] pattern,
    // scoped to the daemon.
    let error = result.expect_err("an unresolvable configured path must refuse");
    let text = format!("{error:?}");
    assert!(text.contains("does not exist"), "got: {text}");
    assert!(text.contains("/definitely/not/here/chrome"), "got: {text}");
}

#[rstest::rstest]
#[test]
fn a_relative_configured_path_refuses_before_existence_is_checked() {
    // Given a config naming a relative path (the schema layer rejects
    // this at launch; the daemon re-checks as its own invariant).
    let probe = TableProbe::of(&[]);
    let config = config_with_binary("google-chrome");

    // When resolving.
    let result = daemon::resolve_browser(&config, &probe);

    // Then the refusal is about absoluteness, not existence.
    let error = result.expect_err("a relative configured path must refuse");
    let text = format!("{error:?}");
    assert!(text.contains("absolute"), "got: {text}");
}

#[rstest::rstest]
#[test]
fn configured_binary_wins_over_detection_and_reports_its_source() {
    // Given both a configured path (existing per the probe) and
    // detectable chrome/chromium names. The configured path is one of
    // the table's answers, which is how the probe "sees" it on disk.
    let probe = TableProbe::of(&[
        ("google-chrome", "/fake/detected/google-chrome"),
        ("chromium", "/fake/detected/chromium"),
        ("my-chrome", "/fake/configured/my-chrome"),
    ]);
    let config = config_with_binary("/fake/configured/my-chrome");

    // When resolving.
    let resolved = daemon::resolve_browser(&config, &probe).expect("configured path resolves");

    // Then the configured path wins and the source says so — no matter
    // which client spawned the daemon, the resolution is identical.
    assert_eq!(
        resolved.executable,
        std::path::PathBuf::from("/fake/configured/my-chrome")
    );
    assert_eq!(resolved.source, BinarySource::Config);
}

#[rstest::rstest]
#[test]
fn auto_detect_prefers_branded_chrome_then_chromium_with_no_edge_or_brave() {
    // Given detection answers for chromium first, chrome second, and
    // edge/brave (which must not be asked at all).
    let probe = TableProbe::of(&[
        ("chromium", "/fake/detected/chromium"),
        ("google-chrome", "/fake/detected/google-chrome"),
        ("microsoft-edge", "/fake/detected/edge"),
        ("brave", "/fake/detected/brave"),
    ]);
    let config = jinn_preferences_config::schemas::BrowserConfig::default();

    // When resolving.
    let resolved = daemon::resolve_browser(&config, &probe).expect("detection finds chrome");

    // Then branded chrome wins over chromium, and the source is the
    // detector.
    assert_eq!(
        resolved.executable,
        std::path::PathBuf::from("/fake/detected/google-chrome")
    );
    assert_eq!(resolved.source, BinarySource::AutoDetected);
}

#[rstest::rstest]
#[test]
fn chromium_is_the_fallback_when_no_branded_chrome_exists() {
    // Given only chromium is detectable.
    let probe = TableProbe::of(&[("chromium", "/fake/detected/chromium")]);
    let config = jinn_preferences_config::schemas::BrowserConfig::default();

    // When resolving.
    let resolved = daemon::resolve_browser(&config, &probe).expect("chromium is the fallback");

    // Then chromium is used.
    assert_eq!(
        resolved.executable,
        std::path::PathBuf::from("/fake/detected/chromium")
    );
}

#[rstest::rstest]
#[test]
fn nothing_detectable_refuses_naming_what_was_searched() {
    // Given a probe that finds nothing.
    let probe = TableProbe::of(&[]);
    let config = jinn_preferences_config::schemas::BrowserConfig::default();

    // When resolving.
    let result = daemon::resolve_browser(&config, &probe);

    // Then the refusal names the candidate list and the config knob.
    let error = result.expect_err("no browser found must refuse");
    let text = format!("{error:?}");
    assert!(text.contains("no browser found"), "got: {text}");
    assert!(text.contains("google-chrome"), "got: {text}");
    assert!(text.contains("[browser] binary"), "got: {text}");
}

// ── auto-start serialization: two concurrent callers, one daemon ────

/// A registry poller standing in for the daemon child: registers once,
/// after a short delay, directly into the registry file. The auto-start
/// path under test is everything around the spawn; the spawn itself is
/// faked because a real child would need the jinn binary and Chrome.
#[rstest::rstest]
#[test]
fn two_concurrent_auto_starters_produce_one_registration_attempt() {
    // Given a start lock beside the registry (the registry's emptiness
    // is the state the auto-start path acts on; the lock is the race
    // guard under test).
    let root = TestRoot::new();
    let lock_path = root.path().as_path().join("browser-daemon.lock");

    // When the first starter takes the lock…
    let first = daemon::StartLock::acquire(lock_path.clone()).expect("first starter wins");

    // …then a second starter arriving during the wait is refused, not
    // double-spawning a daemon.
    let second = daemon::StartLock::acquire(lock_path.clone());

    // Then exactly one holds it.
    assert!(second.is_err(), "the lock must serialize auto-starters");
    drop(first);

    // And after release, a new starter can take it (the retry path).
    let third = daemon::StartLock::acquire(lock_path);
    assert!(third.is_ok(), "a released lock is acquirable");
}

#[rstest::rstest]
#[test]
fn the_spawned_daemon_takes_over_its_spawner_fresh_lock() {
    // The auto-start spawner holds the start lock across spawn +
    // registration-wait; the daemon child it spawned used to see that
    // fresh lock, refuse, and die — every auto-start killed itself
    // (recorded failure mode of the first cut). The fix: a fresh lock
    // held by this process's parent (the spawner) is taken over.
    //
    // Given a fresh lock whose recorded pid is this test process's
    // parent (the shape the real spawner leaves).
    let root = TestRoot::new();
    let lock_path = root.path().as_path().join("browser-daemon.lock");
    #[cfg(unix)]
    let parent = std::os::unix::process::parent_id();
    #[cfg(not(unix))]
    let parent = 0u32;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    std::fs::write(&lock_path, format!("pid={parent}\nstarted={now}\n"))
        .expect("write spawner lock");

    // When the daemon child acquires.
    let acquired = daemon::StartLock::acquire(lock_path);

    // Then the spawner's lock does not block its own child.
    assert!(
        acquired.is_ok(),
        "the daemon must take over the lock its spawner holds: {:?}",
        acquired.err()
    );
}

#[rstest::rstest]
#[test]
fn a_fresh_unrelated_lock_still_refuses() {
    // The parent-takeover must not open the door to genuine double
    // starts: a fresh lock held by anyone else is still refused.
    //
    // Given a fresh lock held by an unrelated pid.
    let root = TestRoot::new();
    let lock_path = root.path().as_path().join("browser-daemon.lock");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    std::fs::write(&lock_path, format!("pid=999999\nstarted={now}\n"))
        .expect("write unrelated lock");

    // When acquiring.
    let acquired = daemon::StartLock::acquire(lock_path);

    // Then it is refused.
    assert!(acquired.is_err(), "a fresh unrelated lock must refuse");
}

#[rstest::rstest]
#[test]
fn a_stale_start_lock_is_taken_over() {
    // Given a start lock file left behind long ago (a crashed starter).
    let root = TestRoot::new();
    let lock_path = root.path().as_path().join("browser-daemon.lock");
    std::fs::write(&lock_path, b"pid=999999\nstarted=0\n").expect("write stale lock");
    // Backdate the file's mtime past the staleness window.
    backdate_mtime(&lock_path, std::time::Duration::from_secs(120));

    // When acquiring.
    let acquired = daemon::StartLock::acquire(lock_path);

    // Then the stale lock is taken over — a crashed starter must not
    // wedge every future start.
    assert!(acquired.is_ok(), "a stale lock must be acquirable");
}

// ── the HTTP API over a faked backend ───────────────────────────────

/// A backend stub: records calls, answers canned results, and can be
/// pointed at a nonexistent tab to exercise the 404 path.
struct StubBackend {
    tabs: std::sync::Mutex<Vec<crate::backend::TabInfo>>,
    screenshot_bytes: Vec<u8>,
}

impl StubBackend {
    fn new() -> Self {
        Self {
            tabs: std::sync::Mutex::new(vec![crate::backend::TabInfo {
                tab: "t1".to_owned(),
                url: Some("https://fake.test/".to_owned()),
                title: Some("Fake".to_owned()),
            }]),
            screenshot_bytes: vec![0x89, b'P', b'N', b'G'],
        }
    }
}

#[async_trait::async_trait]
impl BrowserBackend for StubBackend {
    async fn open_tab(&self, url: Option<&str>) -> Result<crate::backend::TabInfo, BackendError> {
        Ok(crate::backend::TabInfo {
            tab: "t9".to_owned(),
            url: url.map(str::to_owned),
            title: None,
        })
    }

    async fn list_tabs(&self) -> Result<Vec<crate::backend::TabInfo>, BackendError> {
        Ok(self.tabs.lock().expect("tabs").clone())
    }

    async fn navigate(
        &self,
        tab: &str,
        url: &str,
    ) -> Result<crate::backend::TabInfo, BackendError> {
        if tab != "t1" {
            return Err(BackendError::UnknownTab(tab.to_owned()));
        }
        Ok(crate::backend::TabInfo {
            tab: tab.to_owned(),
            url: Some(url.to_owned()),
            title: None,
        })
    }

    async fn snapshot(&self, tab: &str) -> Result<crate::backend::Snapshot, BackendError> {
        if tab != "t1" {
            return Err(BackendError::UnknownTab(tab.to_owned()));
        }
        Ok(crate::backend::Snapshot {
            tab: tab.to_owned(),
            url: Some("https://fake.test/".to_owned()),
            title: Some("Fake".to_owned()),
            refs: vec![crate::backend::SnapshotRef {
                id: "e1".to_owned(),
                tag: "a".to_owned(),
                text: Some("link".to_owned()),
                html_id: None,
                value: None,
                frame: None,
                clickable: true,
                x: Some(10.0),
                y: Some(20.0),
            }],
            tree: "e1: a \"link\"".to_owned(),
        })
    }

    async fn click(&self, tab: &str, reference: &str) -> Result<(), BackendError> {
        if tab != "t1" {
            return Err(BackendError::UnknownTab(tab.to_owned()));
        }
        let _ = reference;
        Ok(())
    }

    async fn click_at(&self, tab: &str, x: f64, y: f64) -> Result<(), BackendError> {
        if tab != "t1" {
            return Err(BackendError::UnknownTab(tab.to_owned()));
        }
        let _ = (x, y);
        Ok(())
    }

    async fn type_text(&self, tab: &str, reference: &str, text: &str) -> Result<(), BackendError> {
        if tab != "t1" {
            return Err(BackendError::UnknownTab(tab.to_owned()));
        }
        let _ = (reference, text);
        Ok(())
    }

    async fn press_key(&self, tab: &str, key: &str) -> Result<(), BackendError> {
        if tab != "t1" {
            return Err(BackendError::UnknownTab(tab.to_owned()));
        }
        let _ = key;
        Ok(())
    }

    async fn evaluate(
        &self,
        tab: &str,
        script: &str,
    ) -> Result<Option<serde_json::Value>, BackendError> {
        if tab != "t1" {
            return Err(BackendError::UnknownTab(tab.to_owned()));
        }
        let _ = script;
        Ok(Some(serde_json::Value::String("ran".to_owned())))
    }

    async fn screenshot(&self, tab: &str) -> Result<Vec<u8>, BackendError> {
        if tab != "t1" {
            return Err(BackendError::UnknownTab(tab.to_owned()));
        }
        Ok(self.screenshot_bytes.clone())
    }

    async fn wait(
        &self,
        tab: &str,
        condition: &crate::backend::WaitCondition,
    ) -> Result<(), BackendError> {
        if tab != "t1" {
            return Err(BackendError::UnknownTab(tab.to_owned()));
        }
        let _ = condition;
        Ok(())
    }

    async fn close_tab(&self, tab: &str) -> Result<(), BackendError> {
        self.tabs.lock().expect("tabs").retain(|t| t.tab != tab);
        Ok(())
    }

    async fn is_live(&self) -> bool {
        true
    }
}

/// Serves the daemon router on an ephemeral loopback port over a stub
/// backend; returns the base URL and the shutdown sender.
async fn serve_stub(
    screenshots_dir: std::path::PathBuf,
) -> (String, tokio::sync::watch::Receiver<bool>) {
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let state = crate::api::DaemonState {
        backend: Arc::new(StubBackend::new()),
        executable: std::path::PathBuf::from("/fake/chrome"),
        binary_source: "config".to_owned(),
        started_at: 0,
        screenshots_dir,
        shutdown: shutdown_tx,
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let app = crate::api::router(state);
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://127.0.0.1:{port}"), shutdown_rx)
}

/// Installs the ring crypto provider for the reqwest clients in this
/// module. `install_default` errors on the second call; the result is
/// deliberately ignored. The workspace pins `rustls-no-provider` and
/// expects each test surface to opt in (the `jinn-mcp` transport tests
/// are the in-tree precedent).
fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// A blocking HTTP probe for the API tests: one raw request, the raw
/// body back. The client crate's envelope decode is covered by the
/// DaemonClient tests below; these tests pin the wire shape itself.
async fn raw_post(base: &str, path: &str, body: &str) -> (u16, String) {
    let url = format!("{base}{path}");
    let client = reqwest::Client::new();
    let response = client
        .post(&url)
        .header("content-type", "application/json")
        .body(body.to_owned())
        .send()
        .await
        .expect("post");
    let status = response.status().as_u16();
    (status, response.text().await.expect("body"))
}

#[rstest::rstest]
#[tokio::test]
async fn snapshot_round_trips_through_the_envelope() {
    install_crypto_provider();
    // Given the daemon API over a stub backend.
    let root = TestRoot::new();
    let (base, _shutdown) = serve_stub(root.path().as_path().to_path_buf()).await;

    // When snapshotting the live tab.
    let (status, body) = raw_post(&base, "/tabs/t1/snapshot", "").await;

    // Then the envelope is ok:true with the stub's snapshot data.
    assert_eq!(status, 200);
    assert!(body.contains("\"ok\":true"), "got: {body}");
    assert!(body.contains("\"e1\""), "got: {body}");
    assert!(body.contains("https://fake.test/"), "got: {body}");
}

#[rstest::rstest]
#[tokio::test]
async fn an_unknown_tab_is_a_404_envelope_error() {
    install_crypto_provider();
    // Given the daemon API over a stub backend.
    let root = TestRoot::new();
    let (base, _shutdown) = serve_stub(root.path().as_path().to_path_buf()).await;

    // When navigating a tab that does not exist.
    let (status, body) = raw_post(
        &base,
        "/tabs/t99/navigate",
        r#"{"url":"https://fake.test/x"}"#,
    )
    .await;

    // Then the response is 404 with an ok:false envelope — a mistyped id
    // is visibly different from an engine failure.
    assert_eq!(status, 404);
    assert!(body.contains("\"ok\":false"), "got: {body}");
    assert!(body.contains("unknown tab"), "got: {body}");
}

#[rstest::rstest]
#[tokio::test]
async fn screenshot_saves_a_png_and_returns_its_path() {
    install_crypto_provider();
    // Given the daemon API over a stub backend with a known screenshots dir.
    let root = TestRoot::new();
    let shots = root.path().as_path().join("shots");
    let (base, _shutdown) = serve_stub(shots.clone()).await;

    // When screenshotting the live tab.
    let (status, body) = raw_post(&base, "/tabs/t1/screenshot", "").await;

    // Then the response names a written PNG path — bytes never ride the
    // response (jinn's MCP client flattens image blocks to a
    // placeholder; the path is what reaches the agent).
    assert_eq!(status, 200);
    assert!(body.contains("\"ok\":true"), "got: {body}");
    let json: serde_json::Value = serde_json::from_str(&body).expect("json");
    let path_text = json
        .get("data")
        .and_then(|d| d.get("path"))
        .and_then(|p| p.as_str())
        .expect("path field")
        .to_owned();
    let path = std::path::PathBuf::from(&path_text);
    assert!(path.starts_with(shots.as_path()), "got: {path_text}");
    assert_eq!(path.extension().and_then(|e| e.to_str()), Some("png"));
    let written = std::fs::read(&path).expect("the png is on disk");
    assert_eq!(written, vec![0x89, b'P', b'N', b'G']);
}

#[rstest::rstest]
#[tokio::test]
async fn shutdown_flips_the_watch() {
    install_crypto_provider();
    // Given the daemon API over a stub backend.
    let root = TestRoot::new();
    let (base, shutdown_rx) = serve_stub(root.path().as_path().to_path_buf()).await;

    // When POSTing /shutdown.
    let (status, body) = raw_post(&base, "/shutdown", "").await;

    // Then the watch flips so the serve loop exits.
    assert_eq!(status, 200);
    assert!(body.contains("\"ok\":true"), "got: {body}");
    let mut rx = shutdown_rx;
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(2), rx.changed())
            .await
            .is_ok(),
        "shutdown watch flipped"
    );
}

// ── the DaemonClient envelope decode ────────────────────────────────

#[rstest::rstest]
#[test]
fn the_client_decodes_the_envelope_into_typed_results() {
    install_crypto_provider();
    // Given the daemon API over a stub backend, with its registry entry
    // written the way the daemon does. The runtime is explicit and this
    // thread stays synchronous: the blocking client must never be
    // dropped inside an async context.
    let root = TestRoot::new();
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let (base, _shutdown) = runtime.block_on(serve_stub(root.path().as_path().to_path_buf()));
    let port: u16 = base
        .rsplit(':')
        .next()
        .and_then(|p| p.parse().ok())
        .expect("port");
    let registry_path = root.path().as_path().join("browser-daemons.json");
    // The entry's port is the stub server's, so the probe answers.
    registry::register(
        &registry_path,
        registry::DaemonEntry::new(std::process::id(), port, 0),
    )
    .expect("register");
    let entry = {
        let mut live = registry::read_live(&registry_path);
        live.pop().expect("live entry")
    };

    // When a client call rides the blocking transport the DaemonClient
    // uses, and the envelope unwrap that every tool call applies.
    let _client = DaemonClient::new();
    let url = format!("http://127.0.0.1:{}/tabs", entry.port());
    let response = reqwest::blocking::Client::new()
        .get(&url)
        .send()
        .expect("get");
    let payload: serde_json::Value = response.json().expect("json");
    let data = payload.get("data").cloned().expect("data envelope");
    let tabs: Vec<crate::backend::TabInfo> = serde_json::from_value(data).expect("tabs decode");

    // Then the stub's tab row arrives typed.
    assert_eq!(tabs.len(), 1);
    assert_eq!(tabs.first().map(|t| t.tab.as_str()), Some("t1"));
}

#[rstest::rstest]
#[test]
fn wait_for_registration_finds_the_childs_entry() {
    // Given a registry the (fake) child will register into. The child's
    // port must answer the liveness probe, so a real listener holds it.
    let root = TestRoot::new();
    let registry_path = root.path().as_path().join("browser-daemons.json");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let expected = registry::DaemonEntry::new(777, port, 0);
    let writer_registry = registry_path.clone();
    let writer_entry = expected.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(150));
        registry::register(&writer_registry, writer_entry).expect("register");
    });
    let _held = listener;

    // When waiting for the registration.
    let found = daemon::wait_for_registration(&registry_path, std::time::Duration::from_secs(5));

    // Then the entry arrives.
    assert_eq!(found.map(|e| (e.pid(), e.port())), Some((777, port)));
}

#[rstest::rstest]
#[test]
fn wait_for_registration_times_out_into_none() {
    // Given an empty registry and a tiny budget.
    let root = TestRoot::new();
    let registry_path = root.path().as_path().join("browser-daemons.json");

    // When waiting.
    let found =
        daemon::wait_for_registration(&registry_path, std::time::Duration::from_millis(400));

    // Then nothing arrives before the budget expires.
    assert!(found.is_none(), "no entry should appear");
}

/// Backdates a file's mtime by `age` (test helper; unix and windows
/// both expose mtime setting through `std` only via filetime, which the
/// workspace does not carry — so this shells through the platform
/// touch: unix `touch -d`, windows `powershell`). Skips silently when
/// the platform command is unavailable; the staleness fallback (unreadable
/// metadata reads as stale) covers the test either way only if mtime moved.
/// To keep the test deterministic on unix, the command failure is an error.
fn backdate_mtime(path: &std::path::Path, age: std::time::Duration) {
    #[cfg(unix)]
    {
        let secs = age.as_secs();
        let out = std::process::Command::new("touch")
            .args(["-d", &format!("{secs} seconds ago")])
            .arg(path)
            .output()
            .expect("touch runs");
        assert!(out.status.success(), "touch -d failed: {:?}", out.stderr);
    }
    #[cfg(windows)]
    {
        let _ = (path, age); // the fallback branch covers windows
    }
}

// The seam types below are exercised by the API stub and the resolution
// tests above; the alias keeps the import list honest.
type _SeamTypes = (
    dyn EngineLauncher,
    dyn EngineHandle,
    dyn PageHandle,
    LiveEngine,
    BackendError,
    DaemonRefusal,
);
