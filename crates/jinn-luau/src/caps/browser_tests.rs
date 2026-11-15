//! The `jinn.browser` family's observable surface: all eleven names are
//! injected, the tab-taking functions require their `tab` field, the
//! wait condition parses its four shapes, and daemon failures surface
//! as Lua errors carrying the daemon's message.
//!
//! The daemon side is mocked with a mockito server whose URL the test
//  writes into the registry the shared client reads — the real
//! discovery path (registry read, port probe) runs against the mock.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "test assertions need expect/panic to state what went wrong"
)]

use std::rc::Rc;

use mlua::Lua;

use super::browser;
use crate::caps::Caps;
use jinn_browser::registry;
use jinn_testutil::TestRoot;
use marked_path::MarkedPathAccess as _;

/// Caps with a plain blocking client (the family builds its own client,
/// so only the deadline/cwd context matters here).
fn browser_caps() -> Rc<Caps> {
    let dir = jinn_testutil::tempdir();
    let client = reqwest::blocking::Client::new();
    Rc::new(Caps::with_http(Vec::new(), dir.path.clone(), None, client))
}

/// Installs a mockito-backed fake daemon: the registry at a hermetic
/// root names the mock's port, so `DaemonClient` discovery answers. The
/// default mock answers every POST with an ok:true envelope.
fn fake_daemon() -> (mockito::ServerGuard, TestRoot, PathsRootGuard) {
    let root = TestRoot::new();
    let mut server = mockito::Server::new();
    // The generic catch-all the per-test mocks fall back to.
    let _any = server
        .mock("POST", mockito::Matcher::Any)
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"ok":true,"data":{"done":true}}"#)
        .expect_at_least(0)
        .create();
    // The mock's URL is http://127.0.0.1:<port>; the client talks to
    // the same loopback host the daemon registry implies.
    let port = server
        .url()
        .rsplit(':')
        .next()
        .and_then(|p| p.parse().ok())
        .expect("mock port");
    // `AppPaths::new_in` nests the registry under <root>/state/jinn/,
    // so the file the client discovery reads is written there — the
    // same construction the daemon tests use.
    let paths = jinn_common::app_paths::AppPaths::new_in(&root.path().as_marked_path());
    let registry_path = paths.browser_daemons_path();
    if let Some(parent) = registry_path.as_path().parent() {
        std::fs::create_dir_all(parent).expect("state dir");
    }
    registry::register(
        registry_path.as_path(),
        registry::DaemonEntry::new(4242, port, 0),
    )
    .expect("register");
    // Point discovery at the hermetic root. `AppPaths::default()` reads
    // the process environment, which is global, so tests that touch it
    // serialize on a static lock and restore the prior value on drop —
    // the `jinn-common` app_paths test guard's shape. Without the lock
    // two parallel tests leave one client reading the other's registry
    // (wrong port → undecodable-envelope failures that vanish solo).
    let env = PathsRootGuard::set(root.path().as_path());
    (server, root, env)
}

/// Serializes every test that points [`PATHS_ROOT_ENV`] at a hermetic
/// root, restoring the prior value on drop (edition 2024 makes
/// `set_var`/`remove_var` unsafe because the environment is
/// process-global).
struct PathsRootGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
    previous: Option<std::ffi::OsString>,
}

/// The env var the browser client's discovery reads.
const PATHS_ROOT_ENV: &str = "JINN_PATHS_ROOT";

/// The lock serializing [`PATHS_ROOT_ENV`] mutation across parallel
/// tests.
static PATHS_ROOT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

impl PathsRootGuard {
    /// Points [`PATHS_ROOT_ENV`] at `root`, holding the lock for the
    /// guard's lifetime.
    fn set(root: &std::path::Path) -> Self {
        let lock = PATHS_ROOT_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let previous = std::env::var_os(PATHS_ROOT_ENV);
        // SAFETY: the lock is held before any env mutation, so no other
        // test mutates or observes the environment concurrently.
        unsafe { std::env::set_var(PATHS_ROOT_ENV, root) };
        Self {
            _lock: lock,
            previous,
        }
    }
}

impl Drop for PathsRootGuard {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => {
                // SAFETY: the guard holds PATHS_ROOT_LOCK for its whole
                // lifetime, so no other test mutates the env mid-restore.
                unsafe { std::env::set_var(PATHS_ROOT_ENV, value) };
            }
            None => {
                // SAFETY: as above.
                unsafe { std::env::remove_var(PATHS_ROOT_ENV) };
            }
        }
    }
}

/// Evaluates a script that calls one `jinn.browser` function and
/// returns its result serialized.
fn run_script(lua: &Lua, script: &str) -> mlua::Result<mlua::Value> {
    let caps = browser_caps();
    let jinn = lua.create_table()?;
    browser::inject(lua, &jinn, &caps)?;
    lua.globals().raw_set("jinn", jinn)?;
    lua.load(script).eval()
}

#[rstest::rstest]
#[test]
fn inject_all_exposes_the_eleven_family_names() {
    // Given an injected VM.
    let lua = Lua::new();
    let caps = browser_caps();
    let jinn = lua.create_table().expect("jinn table");
    browser::inject(&lua, &jinn, &caps).expect("inject");

    // When reading the browser family table.
    let family = jinn.get::<mlua::Table>("browser").expect("family");

    // Then all eleven function names exist.
    for name in [
        "open_tab",
        "list_tabs",
        "navigate",
        "snapshot",
        "click",
        "type",
        "press",
        "evaluate",
        "screenshot",
        "wait",
        "close_tab",
    ] {
        assert!(
            family.get::<mlua::Function>(name).is_ok(),
            "jinn.browser.{name} must be injected"
        );
    }
}

#[rstest::rstest]
#[test]
fn tab_taking_functions_require_the_tab_field() {
    // Given the family injected.
    let lua = Lua::new();
    let caps = browser_caps();
    let jinn = lua.create_table().expect("jinn table");
    browser::inject(&lua, &jinn, &caps).expect("inject");
    lua.globals().raw_set("jinn", jinn).expect("globals");

    // When calling click without a tab (no daemon needed — the field
    // check precedes any HTTP).
    let error = lua
        .load(r#"jinn.browser.click{ ref = "e1" }"#)
        .eval::<mlua::Value>()
        .expect_err("a missing tab is a script error");

    // Then the error names the field.
    assert!(
        error.to_string().contains("missing required field `tab`"),
        "got: {error}"
    );
}

#[rstest::rstest]
#[test]
fn open_tab_returns_the_daemons_tab_row() {
    // Given a fake daemon answering a typed tab row.
    install_ring();
    let (mut server, _root, _env) = fake_daemon();
    let _open = server
        .mock("POST", "/tabs")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"ok":true,"data":{"tab":"t7","url":"https://x.test/","title":null}}"#)
        .create();

    // When a script opens a tab.
    let lua = Lua::new();
    let value = run_script(
        &lua,
        r#"local t = jinn.browser.open_tab{ url = "https://x.test/" }; return t.tab"#,
    )
    .expect("script runs");

    // Then the daemon's id arrives as an ordinary Lua string.
    assert_eq!(
        value,
        mlua::Value::String(lua.create_string("t7").expect("string"))
    );
}

#[rstest::rstest]
#[test]
fn evaluate_returns_the_scripts_value() {
    // Given a fake daemon answering a JSON value.
    install_ring();
    let (mut server, _root, _env) = fake_daemon();
    let _eval = server
        .mock(
            "POST",
            mockito::Matcher::Regex(r"/tabs/t\d+/evaluate".to_owned()),
        )
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"ok":true,"data":"page title"}"#)
        .create();

    // When a script evaluates.
    let lua = Lua::new();
    let value = run_script(
        &lua,
        r#"return jinn.browser.evaluate{ tab = "t1", script = "return document.title" }"#,
    )
    .expect("script runs");

    // Then the decoded value is the string.
    assert_eq!(
        value,
        mlua::Value::String(lua.create_string("page title").expect("string"))
    );
}

#[rstest::rstest]
#[test]
fn a_daemon_error_surfaces_as_a_lua_error_with_the_message() {
    // Given a fake daemon answering an envelope error.
    install_ring();
    let (mut server, _root, _env) = fake_daemon();
    let _err = server
        .mock(
            "POST",
            mockito::Matcher::Regex(r"/tabs/t\d+/navigate".to_owned()),
        )
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"ok":false,"error":"unknown tab: t9"}"#)
        .create();

    // When a script navigates the missing tab.
    let lua = Lua::new();
    let error = run_script(
        &lua,
        r#"jinn.browser.navigate{ tab = "t9", url = "https://x.test/" }"#,
    )
    .expect_err("a daemon error is a script error");

    // Then the daemon's message rides the error.
    assert!(
        error.to_string().contains("unknown tab: t9"),
        "got: {error}"
    );
}

#[rstest::rstest]
#[test]
fn the_wait_condition_accepts_all_four_shapes() {
    // Given a fake daemon whose wait endpoint answers ok.
    install_ring();
    let (mut server, _root, _env) = fake_daemon();
    let _wait = server
        .mock(
            "POST",
            mockito::Matcher::Regex(r"/tabs/t\d+/wait".to_owned()),
        )
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"ok":true,"data":{"done":true}}"#)
        .expect_at_least(4)
        .create();

    // When a script waits with each condition shape, one wrong shape is
    // refused at the family boundary.
    let lua = Lua::new();
    let script = r#"
        jinn.browser.wait{ tab = "t1", condition = "navigation" }
        jinn.browser.wait{ tab = "t1", condition = "network_idle" }
        jinn.browser.wait{ tab = "t1", condition = { text = "Loaded" } }
        jinn.browser.wait{ tab = "t1", condition = { ms = 1500 } }
        local ok, err = pcall(jinn.browser.wait, { tab = "t1", condition = { bogus = 1 } })
        return { ok = ok, err = err }
    "#;
    let value = run_script(&lua, script).expect("script runs");

    // Then the four valid shapes passed and the bogus one raised.
    let result = value_table(&lua, &value);
    assert_eq!(result.get("ok"), Some(&serde_json::Value::Bool(false)));
    let err = result
        .get("err")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned();
    assert!(
        err.contains("`condition` table must carry exactly one of `text` or `ms`"),
        "got: {err}"
    );
}

/// Reads a returned Lua table back as a JSON-ish map for assertions.
fn value_table(lua: &Lua, value: &mlua::Value) -> serde_json::Value {
    let table = match value {
        mlua::Value::Table(t) => t.clone(),
        other => panic!("expected a table, got {other:?}"),
    };
    let mut out = serde_json::Map::new();
    for pair in table.pairs::<mlua::Value, mlua::Value>() {
        let (k, v) = pair.expect("pair");
        let key = match k {
            mlua::Value::String(s) => s.to_str().expect("key str").to_owned(),
            mlua::Value::Integer(i) => i.to_string(),
            other => panic!("unexpected key {other:?}"),
        };
        let val = match v {
            mlua::Value::Boolean(b) => serde_json::Value::Bool(b),
            mlua::Value::String(s) => {
                serde_json::Value::String(s.to_str().expect("val str").to_owned())
            }
            // A caught error arrives as an Error value (Luau mode wraps
            // even runtime errors); read its message.
            mlua::Value::Error(e) => serde_json::Value::String(e.to_string()),
            mlua::Value::Nil => serde_json::Value::Null,
            other => panic!("unexpected value {other:?} in {key}"),
        };
        out.insert(key, val);
    }
    let _ = lua;
    serde_json::Value::Object(out)
}

/// The ring provider install this crate's tests share.
fn install_ring() {
    crate::install_crypto_provider();
}
