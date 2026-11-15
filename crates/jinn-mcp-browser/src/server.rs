//! The browser MCP server handler: eleven tools, all thin clients of
//! the browser daemon's loopback HTTP API.
//!
//! This server process holds no Chrome connection — it is stateless by
//! design, so a config-edit restart of this child (recorded jinn
//! behavior) never recycles the browser. Engine/daemon failures are
//! **tool-level** errors (`CallToolResult::error`): the model reads the
//! message and can act on it. Protocol errors are reserved for requests
//! that cannot be routed at all (unknown tool, malformed arguments).
//!
//! Tab discipline is carried by the tool schemas themselves: every tool
//! except `open_tab` and `list_tabs` requires an explicit `tab` id, and
//! there is no live/current-tab concept anywhere in the surface.

use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ErrorData,
    InitializeResult, ListToolsResult, PaginatedRequestParams, ServerCapabilities, Tool,
};
use rmcp::service::RequestContext;
use serde_json::{Value, json};
use std::sync::Arc;

use jinn_browser::WaitCondition;
use jinn_browser::client::{ClientError, DaemonClient};

/// The eleven tool names, in declaration order — the schema-audit test
/// walks this list, so a new tool added without updating the audit (or
/// vice versa) fails the suite.
pub const TOOL_NAMES: [&str; 11] = [
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
];

/// Tools that name a tab explicitly. Everything else in [`TOOL_NAMES`]
/// (exactly `open_tab` and `list_tabs`) must not require one.
pub(crate) const TAB_TOOLS: [&str; 9] = [
    "navigate",
    "snapshot",
    "click",
    "type",
    "press",
    "evaluate",
    "screenshot",
    "wait",
    "close_tab",
];

/// Converts a JSON schema literal into rmcp's `JsonObject` input-schema
/// form. Panics only on a non-object literal in this file, which would
/// be a bug in the literal itself; `from_value` keeps that failure out
/// of runtime data.
fn to_schema(value: Value) -> rmcp::model::JsonObject {
    match serde_json::from_value(value) {
        Ok(object) => object,
        Err(err) => {
            tracing::error!(%err, "static tool schema literal is not a JSON object");
            rmcp::model::JsonObject::new()
        }
    }
}

/// Merges the shared `tab` property into a schema literal's `properties`
/// and adds `tab` to `required`. Declared once so the required-tab rule
/// cannot drift per tool — and so a tool literal that forgot the tab
/// requirement is impossible: every tab tool's schema is built through
/// here.
fn with_tab(mut schema: Value) -> Value {
    let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) else {
        // An internal invariant: every tab schema literal carries
        // `properties`. A miss is a bug in this file, not runtime state.
        debug_assert!(false, "tab schemas always carry a properties object");
        return schema;
    };
    properties.insert("tab".to_owned(), tab_property());
    if let Some(required) = schema.get_mut("required").and_then(Value::as_array_mut) {
        required.insert(0, Value::String("tab".to_owned()));
    }
    schema
}

/// The `tab` property fragment shared by the nine tab tools.
fn tab_property() -> Value {
    json!({
        "type": "string",
        "description": "The tab id from open_tab or list_tabs \
                       (for example \"t3\"). There is no default \
                       tab: every call names its target."
    })
}

/// The browser MCP server: a stateless client of the browser daemon.
pub struct BrowserMcpServer {
    /// The daemon client; every tool call rides it.
    client: DaemonClient,
}

impl BrowserMcpServer {
    /// Builds the server. Holds no Chrome connection.
    #[must_use]
    pub fn new() -> Self {
        Self {
            client: DaemonClient::new(),
        }
    }

    // ── tool declarations ───────────────────────────────────────────

    pub(crate) fn open_tab_tool() -> Tool {
        Tool::new(
            "open_tab",
            "Open a new tab in the shared browser, optionally navigating \
             it. Returns the new tab id — pass it as `tab` to every other \
             browser tool; nothing acts on a live tab by default.",
            Arc::new(to_schema(json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "The URL to load. Omit (or null) to \
                                       open about:blank and navigate later."
                    }
                }
            }))),
        )
    }

    pub(crate) fn list_tabs_tool() -> Tool {
        Tool::new(
            "list_tabs",
            "List every open tab in the shared browser with its id, URL, \
             and title. Use this to recover a tab id after a restart or \
             to find a tab the user opened by hand.",
            Arc::new(to_schema(json!({
                "type": "object",
                "properties": {}
            }))),
        )
    }

    pub(crate) fn navigate_tool() -> Tool {
        Tool::new(
            "navigate",
            "Navigate an existing tab to a URL. Returns the tab's new URL \
             and title.",
            Arc::new(to_schema(with_tab(json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "The URL to load." }
                },
                "required": ["url"]
            })))),
        )
    }

    pub(crate) fn snapshot_tool() -> Tool {
        Tool::new(
            "snapshot",
            "Capture a readable snapshot of a tab's page: an accessibility- \
             oriented tree plus element refs (e1, e2, …) for every \
             interactive element (links, buttons, inputs, selects, \
             contenteditable, ARIA roles). Pass a ref to click or type; \
             refs are valid until the DOM changes — take a fresh snapshot \
             when one goes stale.",
            Arc::new(to_schema(with_tab(json!({
                "type": "object",
                "properties": {},
                "required": []
            })))),
        )
    }

    pub(crate) fn click_tool() -> Tool {
        Tool::new(
            "click",
            "Click an element by ref at its real coordinates (trusted, \
             human-paced input — not synthetic DOM events). Scrolls the \
             element into view first.",
            Arc::new(to_schema(with_tab(json!({
                "type": "object",
                "properties": {
                    "ref": {
                        "type": "string",
                        "description": "The element ref from the latest \
                                       snapshot of this tab (e1, e2, …)."
                    }
                },
                "required": ["ref"]
            })))),
        )
    }

    pub(crate) fn type_tool() -> Tool {
        Tool::new(
            "type",
            "Type text into an element: clicks its ref to focus, then \
             types per-key (trusted, human-paced input). For plain text \
             fields; press Enter afterward with the press tool if the \
             form needs it.",
            Arc::new(to_schema(with_tab(json!({
                "type": "object",
                "properties": {
                    "ref": {
                        "type": "string",
                        "description": "The element ref from the latest \
                                       snapshot of this tab."
                    },
                    "text": { "type": "string", "description": "The text to type." }
                },
                "required": ["ref", "text"]
            })))),
        )
    }

    pub(crate) fn press_tool() -> Tool {
        Tool::new(
            "press",
            "Press a key in a tab (for example Enter, Tab, Escape, \
             ArrowDown, or a chord like Control+a). Key names follow \
             Chromium's key vocabulary: \"Enter\", \"Tab\", \"Escape\", \
             \"ArrowDown\", \"Control+a\".",
            Arc::new(to_schema(with_tab(json!({
                "type": "object",
                "properties": {
                    "key": {
                        "type": "string",
                        "description": "The key or chord, e.g. \"Enter\", \
                                       \"ArrowDown\", \"Control+a\"."
                    }
                },
                "required": ["key"]
            })))),
        )
    }

    pub(crate) fn evaluate_tool() -> Tool {
        Tool::new(
            "evaluate",
            "Evaluate JavaScript in a tab and return its JSON result. The \
             script rides the stealth evaluation path (no Runtime.enable \
             domain, an isolated on-demand world). Both shapes work: a \
             multi-statement body with `return document.title`, or a bare \
             expression like `document.title` (its value is returned \
             directly). Script exceptions surface as tool errors naming \
             the JS error.",
            Arc::new(to_schema(with_tab(json!({
                "type": "object",
                "properties": {
                    "script": {
                        "type": "string",
                        "description": "The JavaScript to evaluate: either \
                                       statements ending in `return <value>`, \
                                       or a single bare expression. Its \
                                       JSON-serializable value is returned."
                    }
                },
                "required": ["script"]
            })))),
        )
    }

    pub(crate) fn screenshot_tool() -> Tool {
        Tool::new(
            "screenshot",
            "Screenshot a tab. The PNG is saved under jinn's browser \
             screenshots directory and the tool returns the file path — \
             view it with the image-attachment built-in rather than \
             expecting image bytes here.",
            Arc::new(to_schema(with_tab(json!({
                "type": "object",
                "properties": {},
                "required": []
            })))),
        )
    }

    pub(crate) fn wait_tool() -> Tool {
        Tool::new(
            "wait",
            "Wait for a condition in a tab before continuing. The \
             condition is one of: \"navigation\" (the next page load), \
             \"network_idle\" (the page goes quiet), {\"text\": \"...\"} \
             (a substring appears in the page text), or {\"ms\": n} (a \
             fixed duration). Cuts a separate poll-sleep loop out of \
             your tool calls.",
            Arc::new(to_schema(with_tab(json!({
                "type": "object",
                "properties": {
                    "condition": {
                        "description": "One of: \"navigation\", \
                                       \"network_idle\", {\"text\": \"…\"}, \
                                       or {\"ms\": 1500}.",
                        "oneOf": [
                            { "type": "string", "enum": ["navigation", "network_idle"] },
                            {
                                "type": "object",
                                "properties": {
                                    "text": { "type": "string" }
                                },
                                "required": ["text"]
                            },
                            {
                                "type": "object",
                                "properties": {
                                    "ms": { "type": "integer", "minimum": 0 }
                                },
                                "required": ["ms"]
                            }
                        ]
                    }
                },
                "required": ["condition"]
            })))),
        )
    }

    pub(crate) fn close_tab_tool() -> Tool {
        Tool::new(
            "close_tab",
            "Close a tab. The only tab-lifecycle cleanup in the surface — \
             a tab the user closes by hand simply starts answering \
             unknown tab.",
            Arc::new(to_schema(with_tab(json!({
                "type": "object",
                "properties": {},
                "required": []
            })))),
        )
    }

    // ── argument helpers ────────────────────────────────────────────

    fn args(request: &CallToolRequestParams) -> Result<&serde_json::Map<String, Value>, ErrorData> {
        request
            .arguments
            .as_ref()
            .ok_or_else(|| ErrorData::invalid_params("missing tool arguments", None))
    }

    fn required_string<'a>(
        args: &'a serde_json::Map<String, Value>,
        field: &str,
    ) -> Result<&'a str, ErrorData> {
        args.get(field).and_then(Value::as_str).ok_or_else(|| {
            ErrorData::invalid_params(format!("missing string field `{field}`"), None)
        })
    }

    fn optional_string<'a>(
        args: &'a serde_json::Map<String, Value>,
        field: &str,
    ) -> Result<Option<&'a str>, ErrorData> {
        match args.get(field) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) => Ok(Some(s.as_str())),
            Some(_) => Err(ErrorData::invalid_params(
                format!("`{field}` must be a string"),
                None,
            )),
        }
    }

    /// Parses the wait tool's `condition` argument into the backend's
    /// `WaitCondition` shape.
    fn wait_condition(args: &serde_json::Map<String, Value>) -> Result<WaitCondition, ErrorData> {
        let value = args
            .get("condition")
            .ok_or_else(|| ErrorData::invalid_params("missing field `condition`", None))?;
        match value {
            Value::String(s) => match s.as_str() {
                "navigation" => Ok(WaitCondition::Navigation),
                "network_idle" => Ok(WaitCondition::NetworkIdle),
                other => Err(ErrorData::invalid_params(
                    format!(
                        "`condition` string must be \"navigation\" or \
                         \"network_idle\"; got \"{other}\""
                    ),
                    None,
                )),
            },
            Value::Object(map) => {
                if let Some(text) = map.get("text") {
                    let text = text.as_str().ok_or_else(|| {
                        ErrorData::invalid_params("`condition.text` must be a string", None)
                    })?;
                    return Ok(WaitCondition::Text(text.to_owned()));
                }
                if let Some(ms) = map.get("ms") {
                    let ms = ms.as_u64().ok_or_else(|| {
                        ErrorData::invalid_params(
                            "`condition.ms` must be a non-negative integer",
                            None,
                        )
                    })?;
                    return Ok(WaitCondition::Ms(ms));
                }
                Err(ErrorData::invalid_params(
                    "`condition` object must carry exactly one of `text` or `ms`",
                    None,
                ))
            }
            _ => Err(ErrorData::invalid_params(
                "`condition` must be a string or an object with `text`/`ms`",
                None,
            )),
        }
    }

    /// Runs one daemon call on the blocking client, rendering client
    /// failures as tool-level error content the model can read. The
    /// blocking client is parked on a dedicated thread so the async
    /// handler never blocks the rmcp runtime.
    async fn daemon_call<T, F>(&self, tool: &'static str, call: F) -> CallToolResponse
    where
        T: rmcp::serde::Serialize + Send + 'static,
        F: FnOnce(&DaemonClient) -> Result<T, error_stack::Report<ClientError>> + Send + 'static,
    {
        let client = self.client.clone();
        let result = tokio::task::spawn_blocking(move || call(&client))
            .await
            .unwrap_or_else(|e| {
                Err(error_stack::Report::new(ClientError::Unavailable(format!(
                    "daemon call task failed: {e}"
                ))))
            });
        match result {
            Ok(value) => {
                let text = serde_json::to_string(&value).unwrap_or_else(|_| "null".to_owned());
                CallToolResult::success(vec![ContentBlock::text(text)]).into()
            }
            Err(report) => {
                let message = report.current_context().to_string();
                tracing::warn!(tool, message = %message, "browser tool failed");
                CallToolResult::error(vec![ContentBlock::text(format!(
                    "{tool} failed: {message}"
                ))])
                .into()
            }
        }
    }

    // ── tool runs ───────────────────────────────────────────────────

    pub(crate) async fn run_open_tab(
        &self,
        args: Option<&serde_json::Map<String, Value>>,
    ) -> Result<CallToolResponse, ErrorData> {
        let url = match args {
            None => None,
            Some(map) => Self::optional_string(map, "url")?,
        };
        let url = url.map(str::to_owned);
        Ok(self
            .daemon_call("open_tab", move |client| client.open_tab(url.as_deref()))
            .await)
    }

    pub(crate) async fn run_list_tabs(&self) -> CallToolResponse {
        self.daemon_call("list_tabs", DaemonClient::list_tabs).await
    }

    pub(crate) async fn run_navigate(
        &self,
        args: &serde_json::Map<String, Value>,
    ) -> Result<CallToolResponse, ErrorData> {
        let tab = Self::required_string(args, "tab")?.to_owned();
        let url = Self::required_string(args, "url")?.to_owned();
        Ok(self
            .daemon_call("navigate", move |client| client.navigate(&tab, &url))
            .await)
    }

    pub(crate) async fn run_snapshot(
        &self,
        args: &serde_json::Map<String, Value>,
    ) -> Result<CallToolResponse, ErrorData> {
        let tab = Self::required_string(args, "tab")?.to_owned();
        Ok(self
            .daemon_call("snapshot", move |client| client.snapshot(&tab))
            .await)
    }

    pub(crate) async fn run_click(
        &self,
        args: &serde_json::Map<String, Value>,
    ) -> Result<CallToolResponse, ErrorData> {
        let tab = Self::required_string(args, "tab")?.to_owned();
        let reference = Self::required_string(args, "ref")?.to_owned();
        Ok(self
            .daemon_call("click", move |client| client.click(&tab, &reference))
            .await)
    }

    pub(crate) async fn run_type(
        &self,
        args: &serde_json::Map<String, Value>,
    ) -> Result<CallToolResponse, ErrorData> {
        let tab = Self::required_string(args, "tab")?.to_owned();
        let reference = Self::required_string(args, "ref")?.to_owned();
        let text = Self::required_string(args, "text")?.to_owned();
        Ok(self
            .daemon_call("type", move |client| {
                client.type_text(&tab, &reference, &text)
            })
            .await)
    }

    pub(crate) async fn run_press(
        &self,
        args: &serde_json::Map<String, Value>,
    ) -> Result<CallToolResponse, ErrorData> {
        let tab = Self::required_string(args, "tab")?.to_owned();
        let key = Self::required_string(args, "key")?.to_owned();
        Ok(self
            .daemon_call("press", move |client| client.press(&tab, &key))
            .await)
    }

    pub(crate) async fn run_evaluate(
        &self,
        args: &serde_json::Map<String, Value>,
    ) -> Result<CallToolResponse, ErrorData> {
        let tab = Self::required_string(args, "tab")?.to_owned();
        let script = Self::required_string(args, "script")?.to_owned();
        Ok(self
            .daemon_call("evaluate", move |client| client.evaluate(&tab, &script))
            .await)
    }

    pub(crate) async fn run_screenshot(
        &self,
        args: &serde_json::Map<String, Value>,
    ) -> Result<CallToolResponse, ErrorData> {
        let tab = Self::required_string(args, "tab")?.to_owned();
        Ok(self
            .daemon_call("screenshot", move |client| client.screenshot(&tab))
            .await)
    }

    pub(crate) async fn run_wait(
        &self,
        args: &serde_json::Map<String, Value>,
    ) -> Result<CallToolResponse, ErrorData> {
        let tab = Self::required_string(args, "tab")?.to_owned();
        let condition = Self::wait_condition(args)?;
        Ok(self
            .daemon_call("wait", move |client| client.wait(&tab, &condition))
            .await)
    }

    pub(crate) async fn run_close_tab(
        &self,
        args: &serde_json::Map<String, Value>,
    ) -> Result<CallToolResponse, ErrorData> {
        let tab = Self::required_string(args, "tab")?.to_owned();
        Ok(self
            .daemon_call("close_tab", move |client| client.close_tab(&tab))
            .await)
    }
}

impl Default for BrowserMcpServer {
    fn default() -> Self {
        Self::new()
    }
}

impl ServerHandler for BrowserMcpServer {
    fn get_info(&self) -> InitializeResult {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
    }

    // Trait-required async signature; listing is static and never awaits.
    #[expect(clippy::unused_async_trait_impl, reason = "trait signature is async")]
    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<rmcp::service::RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
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

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<rmcp::service::RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        // Unknown tools cannot be routed at all — a protocol error, not
        // a tool-level one.
        match request.name.as_ref() {
            "open_tab" => self.run_open_tab(request.arguments.as_ref()).await,
            "list_tabs" => Ok(self.run_list_tabs().await),
            "navigate" => self.run_navigate(Self::args(&request)?).await,
            "snapshot" => self.run_snapshot(Self::args(&request)?).await,
            "click" => self.run_click(Self::args(&request)?).await,
            "type" => self.run_type(Self::args(&request)?).await,
            "press" => self.run_press(Self::args(&request)?).await,
            "evaluate" => self.run_evaluate(Self::args(&request)?).await,
            "screenshot" => self.run_screenshot(Self::args(&request)?).await,
            "wait" => self.run_wait(Self::args(&request)?).await,
            "close_tab" => self.run_close_tab(Self::args(&request)?).await,
            other => Err(ErrorData::invalid_params(
                format!(
                    "unknown tool `{other}`; expected one of: {}",
                    TOOL_NAMES.join(", ")
                ),
                None,
            )),
        }
    }
}

impl BrowserMcpServer {
    /// One tool's input schema as JSON, by name — the audit path the
    /// schema test walks. An unknown name (a bug in the audit list,
    /// not runtime state) logs a warning and returns `Null`.
    #[must_use]
    pub fn tool_schema(name: &str) -> Value {
        let tool = match name {
            "open_tab" => Self::open_tab_tool(),
            "list_tabs" => Self::list_tabs_tool(),
            "navigate" => Self::navigate_tool(),
            "snapshot" => Self::snapshot_tool(),
            "click" => Self::click_tool(),
            "type" => Self::type_tool(),
            "press" => Self::press_tool(),
            "evaluate" => Self::evaluate_tool(),
            "screenshot" => Self::screenshot_tool(),
            "wait" => Self::wait_tool(),
            "close_tab" => Self::close_tab_tool(),
            // The audit list is a const array in this file; an unknown
            // name is a compile-time-desk-checkable bug, not runtime
            // state, so the unknown arm returns an empty schema rather
            // than panicking mid-audit.
            other => {
                tracing::warn!("tool_schema called for an undeclared tool: {other}");
                return Value::Null;
            }
        };
        Value::Object((*tool.input_schema).clone())
    }
}

/// Witness that the schema-audit constants stay in sync with the
/// declared tool list — a compile-time canary for the test below.
const _: () = {
    assert!(
        TOOL_NAMES.len() == 11,
        "the browser surface is eleven tools"
    );
    assert!(TAB_TOOLS.len() == 9, "nine of the eleven tools name a tab");
};
