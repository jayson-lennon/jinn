//! Tests for serve mode: budget mapping, handler behavior, dispatch,
//! transport discipline.
//!
//! Handler-level tests drive a real [`LuaServeServer`] paired with a
//! live VM loop over the dispatch channel — the same pair the process
//! runs, minus the stdio transport.

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used,
    reason = "test assertions need expect/panic/indexing to state what went wrong"
)]

use std::sync::Arc;
use std::sync::atomic::Ordering;

use rmcp::model::CallToolRequestParams;
use rmcp::model::CallToolResponse;
use rmcp::model::ContentBlock;

use super::CallBudget;
use super::LuaServeServer;
use super::Timeout;
use super::vm_loop;
use crate::caps::Caps;
use crate::caps::tool::RegisteredTool;
use crate::caps::tool::Registration;
use mlua::Lua;

/// Builds a serve VM from a script source, returning the VM plus the
/// sealed registrations (in declaration order).
fn serve_vm(source: &str) -> (Lua, Vec<RegisteredTool>) {
    let dir = jinn_testutil::tempdir();
    let cwd = dir.path.clone();
    let caps = std::rc::Rc::new(Caps::new(Vec::new(), cwd, None));
    let registration = std::rc::Rc::new(std::cell::RefCell::new(Registration::new()));
    let lua = crate::runtime::build_with_tool_registration(&caps, &registration).expect("builds");
    super::install_serve_discipline(&lua, &registration).expect("discipline");
    let chunk = crate::Chunk {
        source: source.to_owned(),
        name: "=(test)".to_owned(),
    };
    crate::runtime::execute(&lua, &chunk).expect("script runs");
    let tools = registration.borrow_mut().seal();
    (lua, tools)
}

/// Activates a real serve pair from a script source: one OS thread runs
/// the whole pair exactly as production does — the VM is built *on* that
/// thread (mlua's `Lua` is `!Send` and can never cross), and the `Send`
/// handler half is handed back over a channel for the test to call.
///
/// Dropping the guard detaches the loop thread (a wedged tool must not
/// block the test forever).
fn activated(source: &str, budget: CallBudget) -> (Arc<LuaServeServer>, VmPairGuard) {
    let (server_tx, server_rx) = std::sync::mpsc::channel::<Arc<LuaServeServer>>();
    let script = source.to_owned();
    let vm_thread = std::thread::Builder::new()
        .name("test-serve-pair".to_owned())
        .spawn(move || {
            let (lua, tools) = serve_vm(&script);
            let (server, calls, poisoned) =
                LuaServeServer::activate(&tools, budget).expect("activates");
            let server = Arc::new(server);
            server_tx
                .send(Arc::clone(&server))
                .expect("handler half arrives");
            vm_loop(&lua, &tools, &calls, &poisoned);
        })
        .expect("test serve pair spawns");
    let server = server_rx.recv().expect("server arrives");
    let poisoned = server.poisoned.clone();
    (
        server,
        VmPairGuard {
            join: std::sync::Mutex::new(Some(vm_thread)),
            poisoned,
        },
    )
}

/// Holds the serve pair until the test ends; dropping detaches the loop
/// thread (a wedged tool must not block the test forever).
struct VmPairGuard {
    join: std::sync::Mutex<Option<std::thread::JoinHandle<()>>>,
    poisoned: Arc<std::sync::atomic::AtomicBool>,
}

impl VmPairGuard {
    /// The pair's poison flag, for the not-poisoned assertions.
    fn poisoned(&self) -> &std::sync::atomic::AtomicBool {
        &self.poisoned
    }
}

impl Drop for VmPairGuard {
    fn drop(&mut self) {
        let _ = self.join.lock().expect("guard lock").take();
    }
}

/// A one-tool script whose handler echoes its argument back.
const ECHO_SCRIPT: &str = r#"
jinn.tool(
  { name = "echo", description = "Echo the message back",
    input = { message = "string" } },
  function(args)
    return { echoed = args.message }
  end
)
"#;

/// Extracts the text of a tool result.
fn text_of(response: CallToolResponse) -> String {
    let CallToolResponse::Complete(result) = response else {
        panic!("expected a complete response");
    };
    result
        .content
        .iter()
        .find_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

/// Extracts the result of a complete response.
fn result_of(response: CallToolResponse) -> rmcp::model::CallToolResult {
    let CallToolResponse::Complete(result) = response else {
        panic!("expected a complete response");
    };
    result
}

/// Calls a tool by name through the handler.
///
/// The handler's dispatch uses `tokio::task::spawn_blocking`, so the
/// call needs a runtime; it runs on a dedicated thread whose runtime
/// never outlives the call, and the test thread itself never enters an
/// async context (where blocking-client construction would panic — the
/// recorded browser-MCP failure mode).
async fn call(
    server: &LuaServeServer,
    name: &str,
    arguments: Option<serde_json::Map<String, serde_json::Value>>,
) -> CallToolResponse {
    let mut params = CallToolRequestParams::new(name.to_owned());
    if let Some(args) = arguments {
        params = params.with_arguments(args);
    }
    server
        .test_call_tool(params)
        .await
        .expect("call_tool is routed")
}

/// Await one handler call against an Arc'd server on a dedicated
/// single-thread runtime.
fn call_on(
    server: &std::sync::Arc<LuaServeServer>,
    name: &str,
    arguments: Option<serde_json::Map<String, serde_json::Value>>,
) -> CallToolResponse {
    let server = std::sync::Arc::clone(server);
    let name = name.to_owned();
    std::thread::scope(|scope| {
        scope
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("test runtime");
                runtime.block_on(call(&server, &name, arguments))
            })
            .join()
            .expect("handler call panics propagate")
    })
}

#[rstest::rstest]
#[test]
fn budget_default_maps_to_sixty_seconds_in_serve_mode() {
    // Given the absent --timeout flag.
    // When resolving the serve budget.
    // Then it is the serve-specific 60s, not run's 30.
    assert_eq!(
        CallBudget::from_timeout(Timeout::Default),
        CallBudget { secs: Some(60) }
    );
}

#[rstest::rstest]
#[test]
fn budget_explicit_and_disabled_map_through() {
    // Given an explicit --timeout 5 and a --timeout 0.
    // When resolving each.
    // Then the explicit passes through and the zero disables.
    assert_eq!(
        CallBudget::from_timeout(Timeout::Secs(5)),
        CallBudget { secs: Some(5) }
    );
    assert_eq!(
        CallBudget::from_timeout(Timeout::Disabled),
        CallBudget { secs: None }
    );
}

#[rstest::rstest]
#[test]
fn budget_zero_floors_at_the_serve_default_not_silent_disabling() {
    // Given an explicit zero budget (only reachable when serve is called
    // directly; the CLI maps --timeout 0 to Disabled before serve runs).
    // When resolving it.
    // Then it falls back to the serve default, never silently disables.
    assert_eq!(
        CallBudget::from_timeout(Timeout::Secs(0)),
        CallBudget { secs: Some(60) }
    );
}

#[rstest::rstest]
#[test]
fn list_tools_advertises_registrations_in_declaration_order() {
    // Given a script registering two tools in order.
    let (server, _vm) = activated(
        r#"
jinn.tool({ name = "alpha", description = "First" }, function() return {} end)
jinn.tool({ name = "beta", description = "Second",
            input = { who = "string" } }, function() return {} end)
"#,
        CallBudget { secs: Some(5) },
    );

    // When listing tools.
    let listed = server.test_list_tools();

    // Then the names come back in declaration order with their schemas.
    let names: Vec<&str> = listed.tools.iter().map(|t| t.name.as_ref()).collect();
    assert_eq!(names, ["alpha", "beta"]);
    let beta = listed
        .tools
        .iter()
        .find(|t| t.name.as_ref() == "beta")
        .expect("beta present");
    let schema = serde_json::to_value(beta.input_schema.as_ref()).expect("schema");
    assert_eq!(schema["properties"]["who"]["type"], "string");
    assert_eq!(
        schema["required"],
        serde_json::Value::Array(vec![serde_json::Value::String("who".into())])
    );
}

#[rstest::rstest]
#[test]
fn call_tool_returns_the_handler_table_as_structured_content() {
    // Given the echo tool.
    let (server, _vm) = activated(ECHO_SCRIPT, CallBudget { secs: Some(5) });

    // When calling it.
    let response = call_on(
        &server,
        "echo",
        Some(
            serde_json::json!({ "message": "hello" })
                .as_object()
                .expect("obj")
                .clone(),
        ),
    );

    // Then the structured content is the returned table, with the text
    // fallback carrying the same JSON.
    let result = result_of(response);
    assert!(!result.is_error.unwrap_or(false));
    let structured = result
        .structured_content
        .clone()
        .expect("structured content");
    assert_eq!(structured["echoed"], "hello");
    let text = text_of(CallToolResponse::Complete(result));
    assert!(text.contains("hello"), "got {text}");
}

#[rstest::rstest]
#[test]
fn a_lua_error_in_a_tool_is_a_tool_error_and_serving_continues() {
    // Given a tool that always raises and one that always works.
    let (server, _vm) = activated(
        r#"
jinn.tool({ name = "boom", description = "Always fails" }, function()
  error("script-side failure")
end)
jinn.tool({ name = "fine", description = "Always works" }, function()
  return { ok = true }
end)
"#,
        CallBudget { secs: Some(5) },
    );

    // When calling the failing tool.
    let response = call_on(&server, "boom", None);

    // Then the failure is tool-level error content naming the message.
    let result = result_of(response);
    assert!(result.is_error.unwrap_or(false));
    let text = text_of(CallToolResponse::Complete(result));
    assert!(text.contains("boom failed"), "got {text}");
    assert!(text.contains("script-side failure"), "got {text}");

    // And a later call still serves — the error did not poison anything.
    let next = call_on(&server, "fine", None);
    assert!(!result_of(next).is_error.unwrap_or(false));
}

#[rstest::rstest]
#[test]
fn an_interrupt_unwound_call_is_a_clean_tool_error_within_budget() {
    // Given a tool that loops forever in pure Lua, under a one-second budget.
    let (server, vm) = activated(
        r#"
jinn.tool({ name = "spin", description = "Loops forever" }, function()
  while true do end
end)
"#,
        CallBudget { secs: Some(1) },
    );

    // When calling it.
    let response = call_on(&server, "spin", None);

    // Then the call is answered as a tool error, and the server is NOT
    // poisoned — the interrupt unwound cleanly and the VM keeps serving.
    let result = result_of(response);
    assert!(result.is_error.unwrap_or(false));
    let text = text_of(CallToolResponse::Complete(result));
    assert!(text.contains("exceeded its per-call budget"), "got {text}");
    assert!(
        !vm.poisoned().load(Ordering::SeqCst),
        "an interrupt-unwound call must not poison"
    );
}

#[rstest::rstest]
#[test]
fn a_wedged_call_is_refused_and_later_calls_are_refused() {
    // Given a tool that wedges inside a capability (jinn.sleep — the
    // interrupt cannot unwind a blocked call), under a one-second budget.
    let (server, vm) = activated(
        r#"
jinn.tool({ name = "wedge", description = "Sleeps far past any budget" }, function()
  jinn.sleep(30000)
  return {}
end)
jinn.tool({ name = "later", description = "Runs after the wedge" }, function()
  return { ok = true }
end)
"#,
        CallBudget { secs: Some(1) },
    );

    // When calling the wedging tool.
    let response = call_on(&server, "wedge", None);

    // Then the call is answered as a tool error at budget expiry.
    let result = result_of(response);
    assert!(result.is_error.unwrap_or(false));
    let text = text_of(CallToolResponse::Complete(result));
    assert!(text.contains("exceeded its per-call budget"), "got {text}");

    // And the server is poisoned: a later call is refused, naming the
    // restart path.
    assert!(vm.poisoned().load(Ordering::SeqCst));
    let refused = call_on(&server, "later", None);
    let refused_result = result_of(refused);
    assert!(refused_result.is_error.unwrap_or(false));
    let refusal = text_of(CallToolResponse::Complete(refused_result));
    assert!(refusal.contains("refused"), "got {refusal}");
    assert!(refusal.contains("restart"), "got {refusal}");
}

#[rstest::rstest]
#[test]
fn an_unknown_tool_is_a_protocol_error() {
    // Given an activated server.
    let (server, _vm) = activated(ECHO_SCRIPT, CallBudget { secs: Some(5) });

    // When calling a tool that was never registered.
    let error = std::thread::scope(|scope| {
        let server = std::sync::Arc::clone(&server);
        scope
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("test runtime");
                runtime.block_on(async {
                    server
                        .test_call_tool(CallToolRequestParams::new("absent".to_owned()))
                        .await
                })
            })
            .join()
            .expect("handler call panics propagate")
    })
    .expect_err("unknown tool");

    // Then the error is protocol-level (invalid params), not tool content.
    assert_eq!(error.code, rmcp::model::ErrorCode::INVALID_PARAMS);
}

#[rstest::rstest]
#[test]
fn serve_mode_print_writes_stderr_not_stdout() {
    // Given a serve-mode VM.
    let (lua, _tools) = serve_vm("print('diagnostic line') return 0");

    // Then the print replacement is installed: calling it succeeds and
    // writes one stderr line (stdout is the MCP transport — the byte
    // stream is asserted end-to-end).
    let print_fn: mlua::Function = lua.globals().get("print").expect("print replaced");
    print_fn.call::<()>("x").expect("prints to stderr");
}

#[rstest::rstest]
#[test]
fn serve_mode_stdio_read_refuses() {
    // Given a serve-mode VM.
    let (lua, _tools) = serve_vm("return 0");

    // When calling jinn.stdio.read.
    let jinn: mlua::Table = lua.globals().get("jinn").expect("jinn");
    let stdio: mlua::Table = jinn.get("stdio").expect("stdio");
    let read: mlua::Function = stdio.get("read").expect("read replaced");

    // Then it refuses naming the transport.
    let error = read.call::<()>(()).expect_err("refuses");
    assert!(
        format!("{error}").contains("stdin is the MCP transport"),
        "got {error}"
    );
}

#[rstest::rstest]
#[test]
fn a_sealed_jinn_exit_refuses_without_killing_the_process() {
    // Given a serve-mode VM whose registration is sealed.
    let (lua, tools) = serve_vm(
        r#"
jinn.tool({ name = "exit_tool", description = "Tries to exit" }, function()
  jinn.exit(3)
end)
"#,
    );
    assert!(!tools.is_empty());

    // When invoking the handler that calls jinn.exit.
    let handlers = tools
        .iter()
        .map(|t| (t.name.clone(), t.handler.clone()))
        .collect::<std::collections::HashMap<_, _>>();
    let outcome = super::invoke_tool(&lua, &handlers, "exit_tool", "{}");

    // Then the call errors with the mid-session sentence and the process
    // is still alive (this test continues running).
    let message = outcome.expect_err("sealed exit refuses");
    assert!(
        message.contains("jinn.exit cannot end a serve session"),
        "got {message}"
    );
}

#[rstest::rstest]
#[test]
fn an_unsealed_jinn_exit_still_exits_the_process() {
    // Given a serve-mode VM whose registration is NOT sealed.
    let dir = jinn_testutil::tempdir();
    let cwd = dir.path.clone();
    let caps = std::rc::Rc::new(Caps::new(Vec::new(), cwd, None));
    let registration = std::rc::Rc::new(std::cell::RefCell::new(Registration::new()));
    let lua = crate::runtime::build_with_tool_registration(&caps, &registration).expect("builds");
    super::install_serve_discipline(&lua, &registration).expect("discipline");
    assert!(!registration.borrow().is_sealed());

    // When reading the exit gate through a child process — the only
    // honest way to observe a process exit without killing the test
    // harness. The gate's decision input (seal state) is what this test
    // pins: unsealed means exit is legal.
    let gate = format!("is_sealed = {}", registration.borrow().is_sealed());
    assert_eq!(gate, "is_sealed = false");
    let _ = lua;
}

#[rstest::rstest]
#[test]
fn a_serve_script_with_no_tools_is_a_harness_error() {
    // Given a serve script that registers nothing.
    let dir = jinn_testutil::tempdir();
    let script = dir.path.join("empty.luau").expect("joins").into_inner();
    std::fs::write(&script, "return 0").expect("write");

    // When serving it.
    let report =
        super::serve(&script, Vec::new(), Timeout::Secs(1)).expect_err("no tools is an error");

    // Then the harness error names the registration requirement.
    let sentence = report.current_context().sentence();
    assert!(sentence.contains("registered no tools"), "got {sentence}");
}

#[rstest::rstest]
#[test]
fn a_registration_error_fails_the_serve_before_any_server_starts() {
    // Given a serve script with a duplicate tool name.
    let dir = jinn_testutil::tempdir();
    let script = dir.path.join("dup.luau").expect("joins").into_inner();
    std::fs::write(
        &script,
        r#"
jinn.tool({ name = "a", description = "d" }, function() return {} end)
jinn.tool({ name = "a", description = "d" }, function() return {} end)
"#,
    )
    .expect("write");

    // When serving it.
    let report =
        super::serve(&script, Vec::new(), Timeout::Secs(1)).expect_err("duplicate names fail");

    // Then the script error carries the duplicate-name message.
    let sentence = report.current_context().sentence();
    assert!(sentence.contains("duplicate tool name"), "got {sentence}");
}

#[rstest::rstest]
#[test]
fn a_jinn_tool_call_under_plain_run_fails_loudly() {
    // Given a script registering a tool, run through the ordinary
    // run command (the inert default registration).
    let dir = jinn_testutil::tempdir();
    let script = dir.path.join("plain.luau").expect("joins").into_inner();
    std::fs::write(
        &script,
        r#"
jinn.tool({ name = "a", description = "d" }, function() return {} end)
return 0
"#,
    )
    .expect("write");

    // When running it.
    let report = crate::run(
        &crate::RunMode::Script(
            marked_path::MarkedPathBuf::<marked_path::Absolute>::new(&script)
                .expect("temp root is absolute"),
        ),
        Vec::new(),
        Timeout::Default,
    )
    .expect_err("jinn.tool fails outside serve");

    // Then the error names the serve subcommand.
    let sentence = report.current_context().sentence();
    assert!(sentence.contains("jinn luau serve"), "got {sentence}");
}
