//! End-to-end tests for `jinn luau serve`.
//!
//! Spawns the real `jinn` binary as `jinn luau serve <script>` and
//! drives it with the real [`McpClient`] stdio transport — the exact
//! path production uses when a `[mcp.x]` entry names the serve
//! subcommand. Asserts the script's `jinn.tool` registrations are
//! advertised over the wire and that a tool call round-trips the Lua
//! handler's returned table (including one backed by `jinn.http`
//! against a mock server).

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "integration test: a failed spawn or IO step is the test's failure channel, and schema asserts index JSON they just built"
)]

use marked_path::MarkedPathAccess;
use marked_path::MarkedPathBuf;

use jinn_mcp::client::McpClient;
use jinn_mcp::client::ServerCommand;
use rmcp::model::ContentBlock;
use serde_json::json;

/// Resolves the built `jinn` binary for integration tests.
#[expect(
    clippy::disallowed_types,
    reason = "cargo's CARGO_BIN_EXE plumbing, consumed only as a std::process::Command program"
)]
fn jinn_bin() -> std::path::PathBuf {
    std::env::var_os("CARGO_BIN_EXE_jinn")
        .map(std::path::PathBuf::from)
        .expect("CARGO_BIN_EXE_jinn not set — integration test must run via cargo test")
}

/// A work directory with one serve script in it.
struct ServeScript {
    _dir: jinn_testutil::temp::JinnTempDir,
    path: MarkedPathBuf<marked_path::Absolute>,
}

impl ServeScript {
    fn with_source(source: &str) -> Self {
        let dir = jinn_testutil::tempdir();
        let path = dir
            .path
            .join("tools.luau")
            .expect("temp root joins a bare name");
        std::fs::write(path.as_ref(), source).expect("write the serve script");
        Self { _dir: dir, path }
    }
}

/// Spawns `jinn luau serve <script> [args...]` and completes the MCP
/// handshake over its stdio. No `--db-path`: luau commands are exempt
/// from the debug-build guard.
async fn connect(script: &MarkedPathBuf<marked_path::Absolute>, extra_args: &[&str]) -> McpClient {
    let mut args = vec![
        "luau".to_owned(),
        "serve".to_owned(),
        script.to_string_lossy().into_owned(),
    ];
    args.extend(extra_args.iter().map(|s| (*s).to_owned()));
    let command = ServerCommand {
        program: jinn_bin().to_string_lossy().into_owned(),
        args,
        env: Vec::new(),
    };
    McpClient::connect(&command)
        .await
        .expect("child handshake succeeds")
}

/// Extracts a text content block from a tool result.
fn text_of(result: &rmcp::model::CallToolResult) -> String {
    result
        .content
        .iter()
        .find_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

#[rstest::rstest]
#[tokio::test]
async fn served_child_advertises_the_script_registered_tools() {
    // Given a script registering two tools, served by the real binary.
    let script = ServeScript::with_source(
        r#"
jinn.tool(
  { name = "greet", description = "Greet someone",
    input = { who = "string" } },
  function(args) return { greeting = "hello " .. args.who } end
)
jinn.tool(
  { name = "ping", description = "Always answers pong" },
  function() return { answer = "pong" } end
)
"#,
    );
    let mut client = connect(&script.path, &[]).await;

    // When listing tools.
    let tools = client.list_tools().await.expect("tools/list");

    // Then both registrations come over the wire in declaration order,
    // each with its schema and required fields.
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
    assert_eq!(names, ["greet", "ping"]);

    let greet = tools
        .iter()
        .find(|t| t.name.as_ref() == "greet")
        .expect("greet");
    assert_eq!(greet.description.as_deref(), Some("Greet someone"));
    let schema = serde_json::to_value(greet.input_schema.as_ref()).expect("schema");
    assert_eq!(schema["properties"]["who"]["type"], "string");
    assert_eq!(
        schema["required"],
        serde_json::Value::Array(vec![serde_json::Value::String("who".into())])
    );

    // And the client shuts down cleanly (child exits).
    client.shutdown().await;
}

#[rstest::rstest]
#[tokio::test]
async fn served_child_round_trips_a_tool_call_through_the_lua_handler() {
    // Given the greet tool.
    let script = ServeScript::with_source(
        r#"
jinn.tool(
  { name = "greet", description = "Greet someone",
    input = { who = "string" } },
  function(args) return { greeting = "hello " .. args.who, loud = true } end
)
"#,
    );
    let mut client = connect(&script.path, &[]).await;

    // When calling it.
    let result = client
        .call_tool(
            "greet",
            Some(json!({ "who": "jinn" }).as_object().expect("obj").clone()),
        )
        .await
        .expect("call succeeds");

    // Then the Lua handler's returned table arrives as the structured
    // content, with the JSON as the text fallback.
    assert!(!result.is_error.unwrap_or(false), "{}", text_of(&result));
    let structured = result
        .structured_content
        .clone()
        .expect("structured content");
    assert_eq!(structured["greeting"], "hello jinn");
    assert_eq!(structured["loud"], true);
    assert!(text_of(&result).contains("hello jinn"));

    client.shutdown().await;
}

#[rstest::rstest]
#[tokio::test]
async fn served_child_http_backed_tool_reaches_a_mock_server() {
    // Given a mock HTTP endpoint and a tool whose handler fetches it
    // through jinn.http (the mockito URL rides the trailing args as
    // jinn.args[1], exactly how a `[mcp.x]` entry would pass config).
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("GET", "/api/who")
        .with_status(200)
        .with_body("{\"name\":\"overgraph\"}")
        .create_async()
        .await;
    let script = ServeScript::with_source(
        r#"
local base = jinn.args[1]
jinn.tool(
  { name = "who", description = "Ask the service who it is" },
  function()
    local res = jinn.http.request({ url = base .. "/api/who" })
    return { status = res.status, body = jinn.json.decode(res.body) }
  end
)
"#,
    );

    let mut client = connect(&script.path, &[&server.url()]).await;

    // When calling the http-backed tool.
    let result = client.call_tool("who", None).await.expect("call succeeds");

    // Then the mock was hit and the decoded body came back structured.
    let structured = result.structured_content.expect("structured content");
    assert_eq!(structured["status"], 200);
    assert_eq!(structured["body"]["name"], "overgraph");
    mock.assert_async().await;

    client.shutdown().await;
}

#[rstest::rstest]
#[tokio::test]
async fn a_lua_error_in_a_tool_is_a_wire_tool_error_and_the_server_serves_on() {
    // Given a script with one failing tool and one working tool.
    let script = ServeScript::with_source(
        r#"
jinn.tool(
  { name = "explode", description = "Always fails" },
  function() error("the disk is on fire") end
)
jinn.tool(
  { name = "fine", description = "Always works" },
  function() return { ok = true } end
)
"#,
    );
    let mut client = connect(&script.path, &[]).await;

    // When calling the failing tool.
    let result = client
        .call_tool("explode", None)
        .await
        .expect("the call is routed");

    // Then the failure is tool-level error content on the wire.
    assert!(result.is_error.unwrap_or(false));
    let text = text_of(&result);
    assert!(text.contains("explode failed"), "got {text}");
    assert!(text.contains("the disk is on fire"), "got {text}");

    // And the same server still serves the working tool.
    let next = client.call_tool("fine", None).await.expect("serves on");
    assert!(!next.is_error.unwrap_or(false));

    client.shutdown().await;
}

#[rstest::rstest]
#[tokio::test]
async fn serve_mode_print_reaches_the_child_stderr_not_the_protocol_stream() {
    // Given a script whose registration prints (stderr discipline) and
    // whose tool prints per call.
    let script = ServeScript::with_source(
        r#"
print("registration-time diagnostic")
jinn.tool(
  { name = "chatty", description = "Prints then answers" },
  function()
    print("call-time diagnostic")
    return { heard = true }
  end
)
"#,
    );
    let mut client = connect(&script.path, &[]).await;

    // When calling the tool and reading the captured stderr tail.
    let result = client
        .call_tool("chatty", None)
        .await
        .expect("call succeeds");
    assert!(!result.is_error.unwrap_or(false));

    // Then the diagnostics landed on the captured stderr (the log
    // channel) and the protocol stream stayed clean — the JSON-RPC
    // answers parsed fine throughout, which they could not have if a
    // print had hit stdout.
    let stderr = client.stderr_tail();
    assert!(
        stderr.contains("registration-time diagnostic"),
        "stderr tail: {stderr}"
    );
    assert!(
        stderr.contains("call-time diagnostic"),
        "stderr tail: {stderr}"
    );

    client.shutdown().await;
}

#[rstest::rstest]
#[tokio::test]
async fn a_script_that_registers_nothing_fails_the_handshake_with_a_clear_stderr() {
    // Given a serve script that registers no tools.
    let script = ServeScript::with_source("return 0");

    // When connecting.
    let args = vec![
        "luau".to_owned(),
        "serve".to_owned(),
        script.path.to_string_lossy().into_owned(),
    ];
    let command = ServerCommand {
        program: jinn_bin().to_string_lossy().into_owned(),
        args,
        env: Vec::new(),
    };
    let connect_result = McpClient::connect(&command).await;

    // Then the handshake fails with the captured stderr naming the
    // registration requirement.
    let message = match connect_result {
        // The context renders as its unit name; the interesting
        // sentences (the stderr tail among them) live in the debug
        // render of the report's attachments.
        Err(error) => format!("{error:?}"),
        // The success arm is not a distinct behavior — it only
        // re-establishes the invariant by producing a message the
        // assertion below rejects.
        Ok(mut client) => {
            client.shutdown().await;
            "handshake unexpectedly succeeded (an empty script cannot serve)".to_owned()
        }
    };
    assert!(message.contains("registered no tools"), "got {message}");
}
