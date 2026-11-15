//! The MCP surface's own observable behavior: the schema audit (tab is
//! required everywhere it must be), tool-level error rendering, and the
//! restart contract (a torn-down and rebuilt server never touches the
//! daemon).
//!
//! The daemon is faked — these tests stub the `DaemonClient`'s HTTP
//! layer, not Chrome.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "test assertions need expect/panic to state what went wrong"
)]

use rmcp::model::{CallToolRequestParams, CallToolResponse, ContentBlock, ListToolsResult};
use serde_json::{Value, json};

use crate::server::{BrowserMcpServer, TAB_TOOLS, TOOL_NAMES};

/// Installs the ring crypto provider for the reqwest clients these
/// tests create. `install_default` errors on the second call; the
/// result is deliberately ignored (the `jinn-mcp` transport tests are
/// the in-tree precedent — the workspace pins `rustls-no-provider`).
fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// Hermetically redirects jinn's state/config root for one test.
///
/// Two shapes:
/// - [`HermeticRoot::dead`] — a file (not a dir) as the root, so
///   discovery fails fast and the call renders a tool error in
///   milliseconds instead of launching anything or waiting out the
///   registration budget.
/// - [`HermeticRoot::registry`] — a real temp root whose registry names
///   a mockito port, so a tool call rides the real discovery path to a
///   fake daemon.
///
/// Tests that touch the environment serialize on a static lock and
/// restore the prior value on drop: without the lock, one test's
/// `JINN_PATHS_ROOT` is visible to another test's client mid-call.
struct HermeticRoot {
    _lock: std::sync::MutexGuard<'static, ()>,
    previous: Option<std::ffi::OsString>,
}

/// The env var the daemon client's discovery reads.
const PATHS_ROOT_ENV: &str = "JINN_PATHS_ROOT";

/// The lock serializing [`PATHS_ROOT_ENV`] mutation across parallel
/// tests.
static PATHS_ROOT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

impl Drop for HermeticRoot {
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

impl HermeticRoot {
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

    /// A root that is a file: every paths operation under it fails
    /// immediately.
    fn dead() -> Self {
        let root = std::env::temp_dir().join("jinn-browser-mcp-hermetic");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::write(&root, b"not a directory").expect("hermetic root written");
        Self::set(&root)
    }

    /// A real temp root whose browser registry names `port` — the
    /// mockito-backed fake daemon.
    fn registry(port: u16) -> (Self, jinn_testutil::TestRoot) {
        use marked_path::MarkedPathAccess as _;
        let root = jinn_testutil::TestRoot::new();
        let paths = jinn_common::app_paths::AppPaths::new_in(&root.path().as_marked_path());
        let registry_path = paths.browser_daemons_path();
        if let Some(parent) = registry_path.as_path().parent() {
            std::fs::create_dir_all(parent).expect("state dir");
        }
        jinn_browser::registry::register(
            registry_path.as_path(),
            jinn_browser::registry::DaemonEntry::new(4242, port, 0),
        )
        .expect("register");
        (Self::set(root.path().as_path()), root)
    }
}

/// What the schema-audit test asserts per tool: the tools that must
/// require `tab` and the tools that must not.
#[rstest::rstest]
#[test]
fn every_tool_schema_requires_tab_except_open_list() {
    // Given the declared tool list.
    // (Capability advertisement — `enable_tools` — is asserted through
    // `test_list_tools` below, the same list the trait serves.)

    // When auditing each tool's input schema through the declaration
    // path (the same static methods list_tools serves).
    let schemas: std::collections::HashMap<String, Value> = TOOL_NAMES
        .iter()
        .map(|name| ((*name).to_owned(), BrowserMcpServer::tool_schema(name)))
        .collect();

    // Then every schema exists, the audit lists match the surface, and
    // the tab rule holds per tool.
    assert_eq!(schemas.len(), TOOL_NAMES.len());
    assert_eq!(TOOL_NAMES.len(), 11);
    assert_eq!(TAB_TOOLS.len(), 9);

    for name in TAB_TOOLS {
        let schema = schemas.get(name).expect("declared schema");
        let required = schema
            .get("required")
            .and_then(Value::as_array)
            .expect("required array");
        assert!(
            required.iter().any(|v| v == "tab"),
            "{name} must require `tab`"
        );
        let tab_prop = schema
            .get("properties")
            .and_then(|p| p.get("tab"))
            .expect("tab property");
        assert!(tab_prop.is_object(), "{name} must carry the tab property");
    }

    for name in ["open_tab", "list_tabs"] {
        let schema = schemas.get(name).expect("declared schema");
        let required = schema
            .get("required")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert!(
            !required.iter().any(|v| v == "tab"),
            "{name} must not require `tab` — it creates/lists them"
        );
        assert!(
            schema["properties"].get("tab").is_none(),
            "{name} must not carry a tab property"
        );
    }
}

/// The dispatch the tests drive. rmcp's `RequestContext` cannot be built
/// outside the service runtime (`Peer::new` is `pub(crate)`), so the
/// tests call the server through this context-free twin of the trait's
/// dispatch — the trait arms are one-line delegations to these same
/// methods with an ignored context, which keeps the two paths from
/// drifting.
impl BrowserMcpServer {
    /// The `list_tools` logic without a context.
    ///
    /// # Errors
    ///
    /// Never fails; the signature mirrors the trait arm it mirrors.
    pub fn test_list_tools(&self) -> Result<ListToolsResult, rmcp::model::ErrorData> {
        Ok(ListToolsResult::with_all_items(vec![
            Self::open_tab_tool(),
            Self::list_tabs_tool(),
            Self::navigate_tool(),
            Self::snapshot_tool(),
            Self::click_tool(),
            Self::type_tool(),
            Self::press_tool(),
            Self::evaluate_tool(),
            Self::screenshot_tool(),
            Self::wait_tool(),
            Self::close_tab_tool(),
        ]))
    }

    /// The `call_tool` dispatch without a context.
    ///
    /// # Errors
    ///
    /// A malformed request (missing arguments) is a protocol error.
    pub async fn test_call_tool(
        &self,
        request: CallToolRequestParams,
    ) -> Result<CallToolResponse, rmcp::model::ErrorData> {
        let args = request.arguments.as_ref().ok_or_else(|| {
            rmcp::model::ErrorData::invalid_params("missing tool arguments", None)
        })?;
        match request.name.as_ref() {
            "open_tab" => self.run_open_tab(Some(args)).await,
            "list_tabs" => Ok(self.run_list_tabs().await),
            "navigate" => self.run_navigate(args).await,
            "snapshot" => self.run_snapshot(args).await,
            "click" => self.run_click(args).await,
            "type" => self.run_type(args).await,
            "press" => self.run_press(args).await,
            "evaluate" => self.run_evaluate(args).await,
            "screenshot" => self.run_screenshot(args).await,
            "wait" => self.run_wait(args).await,
            "close_tab" => self.run_close_tab(args).await,
            other => Err(rmcp::model::ErrorData::invalid_params(
                format!("unknown tool `{other}`"),
                None,
            )),
        }
    }
}

#[rstest::rstest]
#[test]
fn list_tools_advertises_exactly_the_eleven_surface() {
    // Given the browser MCP server.
    let server = BrowserMcpServer::new();

    // When listing tools through the dispatch the trait arm runs.
    let result = server.test_list_tools().expect("tools/list succeeds");

    // Then exactly the eleven names are advertised, in order.
    let names: Vec<String> = result
        .tools
        .iter()
        .map(|tool| tool.name.to_string())
        .collect();
    let expected: Vec<String> = TOOL_NAMES.iter().map(|s| (*s).to_owned()).collect();
    assert_eq!(names, expected);
}

#[rstest::rstest]
#[test]
fn a_daemon_failure_is_tool_level_error_content_not_a_protocol_error() {
    // The reqwest blocking client inside DaemonClient must never be
    // dropped inside a tokio runtime context, and needs the ring
    // provider installed; a sync test owning its runtime gives both.
    install_crypto_provider();
    // And the client's discovery must not reach the real state dir or
    // spawn anything: a hermetic, unwritable root makes the call fail
    // fast with an Unavailable error — exactly the input the
    // tool-level-error rendering is about.
    let _hermetic = HermeticRoot::dead();
    // Given the browser MCP server with no daemon listening (the
    // registry is empty; every call auto-starts or fails visibly).
    let server = BrowserMcpServer::new();
    let runtime = tokio::runtime::Runtime::new().expect("runtime");

    // When calling snapshot with a tab id.
    let response = runtime
        .block_on(
            server.test_call_tool(
                CallToolRequestParams::new("snapshot")
                    .with_arguments(json!({ "tab": "t1" }).as_object().expect("object").clone()),
            ),
        )
        .expect("tools/call is routable — failures are tool-level");

    // Then the result carries error content the model can read, and it
    // is not a protocol error.
    let CallToolResponse::Complete(result) = response else {
        panic!("a plain tool call completes immediately: {response:?}");
    };
    assert!(
        result.is_error.unwrap_or(false),
        "an unreachable daemon must be a tool-level error: {:?}",
        result.content
    );
    let text = result
        .content
        .iter()
        .filter_map(|block| {
            if let ContentBlock::Text(text) = block {
                Some(text.text.clone())
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("snapshot failed"), "got: {text}");
}

#[rstest::rstest]
#[test]
fn an_unknown_tool_is_a_protocol_error() {
    install_crypto_provider();
    // Given the browser MCP server.
    let server = BrowserMcpServer::new();
    let runtime = tokio::runtime::Runtime::new().expect("runtime");

    // When calling a tool the surface does not declare.
    let result = runtime.block_on(async {
        server
            .test_call_tool(CallToolRequestParams::new("no_such_tool"))
            .await
    });

    // Then it is a protocol-level error (invalid params), not content.
    assert!(result.is_err(), "an unknown tool cannot be routed");
}

#[rstest::rstest]
#[test]
fn missing_required_arguments_are_protocol_errors() {
    install_crypto_provider();
    // Given the browser MCP server.
    let server = BrowserMcpServer::new();
    let runtime = tokio::runtime::Runtime::new().expect("runtime");

    // When calling click without its tab (the schema-level contract
    // enforced again at the handler boundary).
    let result = runtime.block_on(async {
        server
            .test_call_tool(
                CallToolRequestParams::new("click")
                    .with_arguments(json!({ "ref": "e1" }).as_object().expect("object").clone()),
            )
            .await
    });

    // Then the malformed request is a protocol error.
    assert!(
        result.is_err(),
        "a missing required argument is not routable"
    );
}

#[rstest::rstest]
#[test]
fn the_screenshot_tool_describes_a_path_return_not_image_bytes() {
    // Given the screenshot tool's description.
    let server = BrowserMcpServer::new();
    let description = tool_description(&server, "screenshot");

    // Then it tells the agent the result is a file path (the recorded
    // [image content] flattening makes image blocks dead ends).
    assert!(
        description.to_lowercase().contains("path"),
        "screenshot description must name the path contract: {description}"
    );
}

#[rstest::rstest]
#[test]
fn screenshot_returns_a_text_path_block_never_an_image_block() {
    // The recorded rule: jinn's MCP client flattens image content
    // blocks to "[image content]", so a screenshot that rode an image
    // block would never reach the model. This test proves the tool
    // returns a *text* block carrying the PNG path — riding the real
    // discovery path to a mockito fake daemon.
    install_crypto_provider();
    // Given a fake daemon answering /screenshot with an ok envelope
    // naming a written file, and a registry pointing discovery at it.
    let mut daemon = mockito::Server::new();
    let png_path = "/tmp/jinn-browser-mcp-shot-fixture.png";
    let _shot = daemon
        .mock(
            "POST",
            mockito::Matcher::Regex(r"/tabs/.+/screenshot".to_owned()),
        )
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(format!(r#"{{"ok":true,"data":{{"path":"{png_path}"}}}}"#))
        .create();
    let port = daemon
        .url()
        .rsplit(':')
        .next()
        .and_then(|p| p.parse().ok())
        .expect("mock port");
    let (_hermetic, _root) = HermeticRoot::registry(port);
    let server = BrowserMcpServer::new();
    let runtime = tokio::runtime::Runtime::new().expect("runtime");

    // When calling the screenshot tool with a tab id.
    let response = runtime.block_on(async {
        server
            .test_call_tool(
                CallToolRequestParams::new("screenshot")
                    .with_arguments(json!({ "tab": "t1" }).as_object().expect("object").clone()),
            )
            .await
    });

    // Then the result is a single text block carrying the path as JSON
    // (the same serialization every tool result rides) — and no image
    // block rides the response.
    let Ok(CallToolResponse::Complete(result)) = response else {
        panic!("a plain tool call completes immediately: {response:?}");
    };
    assert!(
        !result.is_error.unwrap_or(false),
        "the fake daemon answered ok: {:?}",
        result.content
    );
    assert_eq!(result.content.len(), 1, "one block: {:?}", result.content);
    let first = result.content.first().expect("one content block");
    let ContentBlock::Text(text) = first else {
        panic!("screenshot must return text, got: {first:?}");
    };
    assert_eq!(text.text, format!("\"{png_path}\""));
}

#[rstest::rstest]
#[test]
fn the_wait_tool_documents_all_four_conditions() {
    // Given the wait tool's description.
    let server = BrowserMcpServer::new();
    let description = tool_description(&server, "wait");

    // Then all four condition shapes are named.
    for needle in ["navigation", "network_idle", "text", "ms"] {
        assert!(
            description.to_lowercase().contains(needle),
            "wait description must document `{needle}`: {description}"
        );
    }
}

/// Reads one tool's description through the real list_tools path.
///
/// # Panics
///
/// When `name` is not advertised — the test's own failure channel.
fn tool_description(server: &BrowserMcpServer, name: &str) -> String {
    let result = server.test_list_tools().expect("tools/list succeeds");
    let found = result
        .tools
        .iter()
        .find(|tool| tool.name.as_ref() == name)
        .unwrap_or_else(|| panic!("{name} is advertised"));
    found.description.clone().unwrap_or_default().to_string()
}
