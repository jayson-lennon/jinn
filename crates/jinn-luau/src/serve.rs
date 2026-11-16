//! Serve mode: run a script once, then serve its `jinn.tool`
//! registrations as an MCP server on stdio.
//!
//! One plain Lua file is one MCP server: the script executes at load
//! time calling `jinn.tool(spec, fn)` per tool, the collected
//! registrations seal, and the process speaks `tools/list` /
//! `tools/call` until the client disconnects. Configuration rides the
//! ordinary `[mcp.x]` machinery (`command = "<jinn>"`,
//! `args = ["luau", "serve", "<session_cwd>/tool.lua"]`).
//!
//! ## Threading model
//!
//! The `mlua::Lua` value is `!Send`, and the crate's VM is deliberately
//! single-threaded (`Rc<Caps>`, no locking). Serve therefore splits the
//! process into two threads:
//!
//! - **VM thread** — owns the `Lua` value and the registered handlers.
//!   One call at a time, drained from an `mpsc` channel: dispatch is
//!   serialized by construction, which also protects any shared backend
//!   the tools drive (the browser daemon, say) from interleaved calls.
//!   Only `String`s (JSON) cross the channel — no mlua type ever
//!   crosses a thread.
//! - **rmcp runtime** — a fresh multi-thread tokio runtime running the
//!   stdio transport. The handler struct (`LuaServeServer`) holds only
//!   `Send + Sync` state: the sealed tool definitions and a cloneable
//!   channel sender.
//!
//! The blocking reqwest client in [`crate::caps::Caps`] is built on the
//! caller's thread *before* any runtime exists (the browser-MCP bug:
//! constructing a blocking client inside a tokio runtime panics at
//! startup).
//!
//! ## Transport discipline
//!
//! stdout and stdin are the MCP JSON-RPC transport, so serve mode
//! replaces `print` (stdout) with a stderr writer, refuses
//! `jinn.stdio.read` (stdin is the transport), and state-gates
//! `jinn.exit` (legal during registration, an error once serving) —
//! one stray line corrupts the protocol stream irrecoverably.
//!
//! Diagnostics are stderr-only: `print`/`jinn.stdio.eprint` land in
//! jinn's captured stderr tail (the MCP picker's `<c-t>` preview).
//! There is deliberately no wire log API — rmcp's logging notification
//! is deprecated (SEP-2577) and jinn's client never renders log
//! notifications.
//!
//! ## Per-call budget
//!
//! Each `tools/call` runs under a budget: the parent `--timeout` flag,
//! defaulting to **60 seconds** in serve mode (`0` disables; the run
//! default stays 30 because there the process *is* the script). The
//! handler's reply wait and the VM's interrupt measure one shared
//! absolute deadline, so they cannot drift apart by a scheduling gap. A
//! call the VM interrupt unwinds (pure Lua running long) is a clean tool
//! error and serving continues — an unwind whose reply lands in the
//! short grace drain just past expiry still counts as answered. A call
//! wedged inside a capability (the interrupt only fires on instruction
//! ticks and cannot unwind a blocked `jinn.http`/`jinn.sleep`) never
//! replies at all: it is answered as a tool error at budget expiry, then
//! **poisons the server** — the wedged VM cannot be trusted to answer
//! anything, so every later call is refused, naming the restart path
//! (`<c-r>` in the MCP picker).
//!
//! ## Error discipline
//!
//! The bundled servers' shared rule: tool failures (a Lua error, a
//! budget overrun) are **tool-level** errors — content the model reads
//! and acts on. Protocol errors (`ErrorData`) are reserved for requests
//! that cannot be routed at all (unknown tool, uninterpretable
//! arguments).

use std::collections::HashMap;
use std::collections::HashSet;
use std::io::Write as _;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::Duration;
use std::time::Instant;

use error_stack::Report;
use mlua::Lua;
use rmcp::handler::server::ServerHandler;
use rmcp::model::CallToolRequestParams;
use rmcp::model::CallToolResponse;
use rmcp::model::CallToolResult;
use rmcp::model::ContentBlock;
use rmcp::model::ErrorData;
use rmcp::model::InitializeResult;
use rmcp::model::ListToolsResult;
use rmcp::model::PaginatedRequestParams;
use rmcp::model::ServerCapabilities;
use rmcp::model::Tool;
use rmcp::service::RequestContext;
use rmcp::service::RoleServer;

use crate::LuauError;
use crate::Timeout;
use crate::caps::Caps;
use crate::caps::tool::RegisteredTool;
use crate::caps::tool::Registration;
use crate::runtime::TIMEOUT_SENTINEL;

/// The serve-mode per-call budget when no `--timeout` was given.
///
/// Deliberately longer than run's 30s: in run mode the process *is* the
/// script, so a timeout can just kill it; in serve mode the process must
/// outlive calls that legitimately take longer than one script run.
const DEFAULT_SERVE_BUDGET_SECS: u64 = 60;

/// The internal marker for a call that got no VM reply (budget expiry,
/// VM death). Never rendered to a caller; matched in `call_tool` to run
/// the poison path.
const WEDGED_SENTINEL: &str = "\u{0}jinn-luau-serve-wedged\u{0}";

/// How long the handler keeps draining past a shared-deadline expiry for
/// the VM's budget-interrupt unwind to land.
///
/// The interrupt fires at the same absolute instant the handler's wait
/// expires (one shared deadline carried in the request), so an
/// interrupt-unwound call's reply trails expiry by microseconds — the
/// unwind, the error render, the JSON encode. The grace drains that
/// window: a reply within it proves the VM stayed responsive (a clean
/// budget error, never a poison), while a call wedged inside a
/// capability never replies at all. Far above unwind cost, far below the
/// smallest capability wait a wedge could be hiding behind.
const WEDGE_GRACE: Duration = Duration::from_millis(250);

/// One in-flight tool dispatch, handler → VM thread.
pub(crate) struct ToolCallRequest {
    /// The tool's registered name.
    pub tool: String,
    /// The call's arguments, JSON-encoded.
    pub arguments: String,
    /// The call's shared absolute deadline: the VM's budget interrupt
    /// fires at it and the handler's reply wait expires at it, so the
    /// two clocks cannot drift by a scheduling gap. `None` when
    /// per-call budgeting is disabled.
    pub deadline: Option<Instant>,
    /// The reply half, dropped on handler timeout (the poison signal).
    pub reply: mpsc::SyncSender<Result<String, String>>,
}

/// The resolved budget state for one serve process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CallBudget {
    /// The budget in seconds; `None` when disabled (`--timeout 0`).
    pub secs: Option<u64>,
}

impl CallBudget {
    /// Resolves the serve per-call budget from the timeout spec.
    ///
    /// The default differs from run mode's 30s (see
    /// [`DEFAULT_SERVE_BUDGET_SECS`]); an explicit `--timeout n` passes
    /// through floored at one second exactly like run mode; `0` disables.
    #[must_use]
    pub fn from_timeout(timeout: Timeout) -> Self {
        let secs = match timeout {
            Timeout::Disabled => None,
            Timeout::Default | Timeout::Secs(0) => Some(DEFAULT_SERVE_BUDGET_SECS),
            Timeout::Secs(n) => Some(n.max(1)),
        };
        Self { secs }
    }

    /// The budget as a [`Duration`], or `None` when per-call budgeting
    /// is disabled.
    #[must_use]
    pub fn duration(self) -> Option<Duration> {
        self.secs.map(Duration::from_secs)
    }
}

/// Serves the script's `jinn.tool` registrations over stdio MCP until
/// the client disconnects.
///
/// The script executes once (with serve-mode transport discipline and a
/// registration deadline), its tools seal, and the process serves until
/// the transport closes. On success the caller should exit 0; on setup
/// failure the [`LuauError`] maps through the intercept's error path
/// (`sentence` + `exit_code`).
///
/// Threading: this thread builds the VM and then *becomes* the VM loop
/// (mlua's `Lua` is `!Send` in this workspace — the VM can never cross a
/// thread), while the rmcp runtime and its `Send` handler run on a
/// spawned thread. Only JSON strings cross between them.
///
/// # Errors
///
/// Returns the error vocabulary ([`LuauError`]) for: an unreadable
/// script, a script that failed to load (syntax, runtime, registration
/// error, timeout, memory), a script that registered zero tools, a
/// schema that failed wire conversion, or a runtime that failed to
/// start.
pub fn serve(script: &Path, args: Vec<String>, timeout: Timeout) -> Result<(), Report<LuauError>> {
    let budget = CallBudget::from_timeout(timeout);
    let chunk = crate::chunk_from_file(script)?;
    let cwd = current_cwd()?;

    // The registration deadline bounds only the load-time execution —
    // a runaway registration script must die like any runaway run.
    // Per-call budgets are installed fresh by the VM loop per call.
    let load_deadline = budget
        .secs
        .map(|secs| Instant::now() + Duration::from_secs(secs));

    let caps = std::rc::Rc::new(Caps::new(args, cwd, load_deadline));
    let registration = std::rc::Rc::new(std::cell::RefCell::new(Registration::new()));
    let lua =
        crate::runtime::build_with_tool_registration(&caps, &registration).map_err(Report::new)?;

    install_serve_discipline(&lua, &registration).map_err(|error| {
        Report::new(LuauError::Harness {
            sentence: format!("cannot install the serve transport discipline: {error}"),
        })
    })?;

    crate::runtime::execute(&lua, &chunk).map_err(|error| {
        crate::commands::script_error(&chunk.name, budget.secs.unwrap_or(0), error)
    })?;

    let tools = registration.borrow_mut().seal();
    if tools.is_empty() {
        return Err(Report::new(LuauError::Harness {
            sentence: format!(
                "the script `{}` registered no tools (jinn.tool); nothing to serve",
                script.display()
            ),
        }));
    }

    let (server, calls, poisoned) = LuaServeServer::activate(&tools, budget)?;

    // The rmcp runtime runs on a spawned thread; its completion signal
    // doubles as the transport-closed reaper below.
    let (done_tx, done_rx) = mpsc::channel::<Result<(), Report<LuauError>>>();
    std::thread::Builder::new()
        .name("jinn-luau-serve-runtime".to_owned())
        .spawn(move || {
            let outcome = serve_with(server);
            let _ = done_tx.send(outcome);
        })
        .map_err(|error| {
            Report::new(LuauError::Harness {
                sentence: format!("cannot start the serve runtime thread: {error}"),
            })
        })?;

    // THIS thread is the VM loop: it owns the `Lua` value and drains one
    // call at a time until every dispatch sender drops (the transport
    // closed and the runtime thread released the server).
    vm_loop(&lua, &tools, &calls, &poisoned);

    // Reap the runtime thread's outcome (the transport-closed result).
    match done_rx.recv() {
        Ok(outcome) => outcome,
        Err(_) => Ok(()), // the runtime thread died silently; treat as close
    }
}

/// Reads the process cwd as the typed owning form `Caps` carries.
fn current_cwd() -> Result<marked_path::MarkedPathBuf<marked_path::Absolute>, Report<LuauError>> {
    let raw = std::env::current_dir().map_err(|error| {
        Report::new(LuauError::Harness {
            sentence: format!("cannot read the process cwd: {error}"),
        })
    })?;
    marked_path::MarkedPathBuf::<marked_path::Absolute>::new(&raw)
        .or_else(|_untyped| {
            marked_path::MarkedPathBuf::<marked_path::Absolute>::new(std::env::temp_dir())
        })
        .map_err(|_temp_dir_also_untyped| {
            Report::new(LuauError::Harness {
                sentence: "the process cwd is not an absolute path".to_owned(),
            })
        })
}

/// Installs serve-mode transport discipline on the VM, before the
/// script's first byte executes.
///
/// - `print` → one stderr line with a flush (registration-time prints
///   included): stdout is the MCP transport.
/// - `jinn.stdio.read` → always errors: stdin is the MCP transport.
/// - `jinn.exit` → state-gated: legal during registration (a script may
///   legitimately refuse to serve — missing env, say), an error once the
///   table is sealed, because exiting kills the server mid-session.
fn install_serve_discipline(
    lua: &Lua,
    registration: &std::rc::Rc<std::cell::RefCell<Registration>>,
) -> mlua::Result<()> {
    // print: same body as jinn.stdio.eprint — one stderr line, flushed.
    let print_fn = lua.create_function(|_lua, message: String| {
        let mut stderr = std::io::stderr();
        writeln!(stderr, "{message}")
            .and_then(|()| stderr.flush())
            .map_err(|error| mlua::Error::runtime(format!("cannot write stderr: {error}")))
    })?;
    lua.globals().raw_set("print", print_fn)?;

    // jinn.stdio.read: refuse — stdin is the transport.
    let read_refuse = lua.create_function(|_lua, ()| -> mlua::Result<()> {
        Err(mlua::Error::runtime(
            "jinn.stdio.read is unavailable in serve mode: stdin is the MCP transport",
        ))
    })?;
    let jinn: mlua::Table = lua.globals().get("jinn")?;
    let stdio: mlua::Table = jinn.get("stdio")?;
    stdio.raw_set("read", read_refuse)?;

    // jinn.exit: state-gated through the registration state.
    let exit_registration = std::rc::Rc::clone(registration);
    #[expect(
        clippy::exit,
        reason = "unsealed jinn.exit during registration IS the script's process exit; the code must reach the shell (same shape as the time family's exit)"
    )]
    let exit_fn = lua.create_function(move |_lua, code: Option<i32>| -> mlua::Result<()> {
        if exit_registration.borrow().is_sealed() {
            return Err(mlua::Error::runtime(
                "jinn.exit cannot end a serve session; return from the tool function instead",
            ));
        }
        let _ = std::io::Write::flush(&mut std::io::stdout());
        let _ = std::io::Write::flush(&mut std::io::stderr());
        std::process::exit(code.unwrap_or_default());
    })?;
    jinn.raw_set("exit", exit_fn)?;
    Ok(())
}

/// Serves an activated server over stdio until the client disconnects.
///
/// Mirrors the browser server's threading shape: the runtime is built
/// *here*, on a thread that never enters an async context, because the
/// [`Caps`] HTTP client (a `reqwest::blocking::Client` whose
/// construction panics inside a tokio runtime) already exists in the
/// VM's closure captures.
fn serve_with(server: LuaServeServer) -> Result<(), Report<LuauError>> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| {
            Report::new(LuauError::Harness {
                sentence: format!("cannot start the serve runtime: {error}"),
            })
        })?;
    runtime.block_on(async {
        let transport = rmcp::transport::io::stdio();
        let running = rmcp::service::ServiceExt::serve(server, transport)
            .await
            .map_err(|error| {
                Report::new(LuauError::Harness {
                    sentence: format!("MCP stdio handshake failed: {error}"),
                })
            })?;
        // Completes when the transport closes (client disconnect / stdin EOF).
        let _ = running.waiting().await;
        Ok(())
    })
}

/// The activated server: sealed tool definitions plus the dispatch
/// channel to the VM thread. `Send + Sync` by construction — no mlua
/// type ever lives here.
struct LuaServeServer {
    /// The sealed tools, declaration order, ready for `tools/list`.
    tools: Arc<[Tool]>,
    /// The registered names, looked up per call.
    known: Arc<HashSet<String>>,
    /// The dispatch channel into the VM thread.
    dispatch: mpsc::Sender<ToolCallRequest>,
    /// Set once a call wedged past its budget; later calls are refused.
    poisoned: Arc<AtomicBool>,
    /// The per-call budget applied to every dispatch.
    budget: CallBudget,
}

impl LuaServeServer {
    /// Wraps the sealed registrations into wire tools plus a dispatch
    /// sender into the VM loop (which the caller's thread is about to
    /// enter).
    ///
    /// Runs on the caller's thread; the returned server is `Send + Sync`
    /// by construction — no mlua type ever lives on it.
    ///
    /// # Errors
    ///
    /// Returns [`LuauError::Harness`] when a registration's schema fails
    /// wire conversion — the server refuses to start rather than
    /// advertise a broken schema.
    fn activate(
        tools: &[RegisteredTool],
        budget: CallBudget,
    ) -> Result<(Self, mpsc::Receiver<ToolCallRequest>, Arc<AtomicBool>), Report<LuauError>> {
        let wire_tools = tools
            .iter()
            .map(|registered| {
                let schema = serde_json::from_value::<rmcp::model::JsonObject>(
                    registered.input_schema.clone(),
                )
                .map_err(|error| {
                    Report::new(LuauError::Harness {
                        sentence: format!(
                            "tool `{}`'s input schema failed wire conversion: {error}",
                            registered.name
                        ),
                    })
                })?;
                Ok::<_, Report<LuauError>>(Tool::new(
                    registered.name.clone(),
                    registered.description.clone(),
                    Arc::new(schema),
                ))
            })
            .collect::<Result<Vec<_>, _>>()?;

        let known = tools
            .iter()
            .map(|registered| registered.name.clone())
            .collect::<HashSet<_>>();

        let (dispatch, calls) = mpsc::channel::<ToolCallRequest>();
        let poisoned = Arc::new(AtomicBool::new(false));
        let server = Self {
            tools: Arc::from(wire_tools),
            known: Arc::new(known),
            dispatch,
            poisoned: Arc::clone(&poisoned),
            budget,
        };
        Ok((server, calls, poisoned))
    }

    /// Renders a refused-call tool error naming the restart path.
    fn poison_error(tool: &str) -> CallToolResponse {
        CallToolResult::error(vec![ContentBlock::text(format!(
            "{tool} refused: an earlier call wedged past its per-call budget; \
             the server will not run further tools — restart the server \
             (<c-r> in the MCP picker)"
        ))])
        .into()
    }
}

/// The VM loop: owns the `Lua` value on the caller's thread, drains one
/// call at a time.
///
/// Each call gets a fresh per-call deadline interrupt (the load-time
/// interrupt bounds only registration). A Lua error — including one
/// raised by the budget interrupt unwinding the call — becomes an
/// `Err` reply and the loop serves on. A wedged call never replies;
/// the handler answers it and poisons the server, and this loop's
/// eventual reply lands in a dropped channel and breaks the loop
/// quietly (the server refuses further calls regardless).
fn vm_loop(
    lua: &Lua,
    tools: &[RegisteredTool],
    calls: &mpsc::Receiver<ToolCallRequest>,
    poisoned: &AtomicBool,
) {
    let handlers: HashMap<String, mlua::Function> = tools
        .iter()
        .map(|registered| (registered.name.clone(), registered.handler.clone()))
        .collect();

    while let Ok(request) = calls.recv() {
        if poisoned.load(Ordering::SeqCst) {
            // The server was poisoned; the loop drains and refuses
            // nothing further — it must still reply (or drop) so the
            // handler's waiter never hangs past its budget.
            continue;
        }

        // Per-call budget interrupt, replacing the load-time one. The
        // deadline is the request's shared absolute instant — the same
        // clock the handler's reply wait reads.
        if let Some(deadline) = request.deadline {
            lua.set_interrupt(move |_lua| {
                if Instant::now() >= deadline {
                    Err(mlua::Error::runtime(TIMEOUT_SENTINEL))
                } else {
                    Ok(mlua::VmState::Continue)
                }
            });
        } else {
            lua.set_interrupt(|_lua| Ok(mlua::VmState::Continue));
        }

        let reply = invoke_tool(lua, &handlers, &request.tool, &request.arguments);

        if request.reply.send(reply).is_err() {
            // The handler gave up on this call (budget expiry) and
            // poisoned the server. Drain quietly; nothing can be trusted.
            break;
        }
    }
}

/// Invokes one registered tool handler and serializes its result table.
///
/// The handler's returned table is the tool's result: JSON-encoded on
/// the VM thread, decoded to `structuredContent` by the handler side
/// with the encoded text as the content fallback. A Lua error (tool
/// failure, budget-interrupt unwind) becomes the error string; the
/// sentinel maps to a budget-specific sentence.
fn invoke_tool(
    lua: &Lua,
    handlers: &HashMap<String, mlua::Function>,
    tool: &str,
    arguments: &str,
) -> Result<String, String> {
    let Some(handler) = handlers.get(tool) else {
        return Err(format!("no tool named `{tool}` is registered"));
    };

    let parsed: serde_json::Value = serde_json::from_str(arguments)
        .map_err(|error| format!("arguments are not valid JSON: {error}"))?;
    let args_table = crate::caps::json::from_json(lua, &parsed)
        .map_err(|error| format!("cannot decode the arguments into Lua: {error}"))?;

    let result = handler
        .call::<mlua::Value>(args_table)
        .map_err(|error| budget_or_script_error(&error))?;

    if result == mlua::Value::Nil {
        // A tool returning nothing is an empty object result, not an error.
        return Ok("{}".to_owned());
    }
    let json = crate::caps::json::to_json(&result)
        .map_err(|error| format!("the tool's result is not JSON-encodable: {error}"))?;
    serde_json::to_string(&json)
        .map_err(|error| format!("cannot serialize the tool's result: {error}"))
}

/// Awaits the VM's reply for one dispatched call.
///
/// The wait is the shared deadline the request carries; on expiry the
/// wait drains one extra [`WEDGE_GRACE`] — an interrupt-unwound call's
/// reply trails expiry only by unwind cost, so one that lands in the
/// grace is answered normally, while a call wedged inside a capability
/// never replies at all. `Ok(reply)` means the VM answered; `Err` means
/// it never did (the poison discriminator).
fn wait_for_reply(
    reply_rx: &mpsc::Receiver<Result<String, String>>,
    deadline: Option<Instant>,
) -> Result<String, String> {
    match deadline {
        Some(deadline) => {
            let now = Instant::now();
            let first_wait = deadline.saturating_duration_since(now);
            if let Ok(reply) = reply_rx.recv_timeout(first_wait) {
                return reply;
            }
            reply_rx
                .recv_timeout(WEDGE_GRACE)
                .unwrap_or_else(|_expired| Err(WEDGED_SENTINEL.to_owned()))
        }
        None => reply_rx
            .recv()
            .unwrap_or_else(|_vm_died| Err(WEDGED_SENTINEL.to_owned())),
    }
}

/// Renders a handler-side Lua error, naming the budget when the
/// interrupt sentinel unwound the call.
fn budget_or_script_error(error: &mlua::Error) -> String {
    let mlua::Error::RuntimeError(message) = error else {
        return format!("{error}");
    };
    if message.contains(super::runtime::TIMEOUT_SENTINEL) {
        "the tool exceeded its per-call budget and was interrupted".to_owned()
    } else {
        message.clone()
    }
}

impl LuaServeServer {
    /// The `list_tools` logic without a context.
    ///
    /// rmcp's `RequestContext` cannot be built outside the service
    /// runtime (`Peer::new` is `pub(crate)`), so tests drive the server
    /// through these context-free twins; the trait arms are one-line
    /// delegations, which keeps the two paths from drifting.
    ///
    pub(crate) fn test_list_tools(&self) -> ListToolsResult {
        ListToolsResult::with_all_items(self.tools.iter().cloned().collect::<Vec<_>>())
    }

    /// The `call_tool` dispatch without a context.
    ///
    /// # Errors
    ///
    /// An unknown tool name is a protocol error.
    pub(crate) async fn test_call_tool(
        &self,
        request: CallToolRequestParams,
    ) -> Result<CallToolResponse, ErrorData> {
        let tool = request.name.as_ref().to_owned();
        if !self.known.contains(&tool) {
            return Err(ErrorData::invalid_params(
                format!("unknown tool `{tool}`"),
                None,
            ));
        }
        if self.poisoned.load(Ordering::SeqCst) {
            return Ok(Self::poison_error(&tool));
        }

        let arguments = request.arguments.as_ref().map_or_else(
            || "{}".to_owned(),
            |args| serde_json::Value::Object(args.clone()).to_string(),
        );
        let budget = self.budget.duration();

        // The VM handoff runs on the blocking pool so the async runtime
        // never blocks on the VM thread (the daemon_call shape). The
        // reply wait races the call's budget: an expired budget answers
        // the tool error and poisons the server, because the wedged VM
        // (blocked inside a capability the interrupt cannot unwind) can
        // no longer be trusted to answer anything.
        let dispatch = self.dispatch.clone();
        let tool_name_clone = tool.clone();
        let deadline = budget.map(|b| Instant::now() + b);
        let result = tokio::task::spawn_blocking(move || -> Result<String, String> {
            let (reply_tx, reply_rx) = mpsc::sync_channel::<Result<String, String>>(1);
            dispatch
                .send(ToolCallRequest {
                    tool: tool.clone(),
                    arguments,
                    deadline,
                    reply: reply_tx,
                })
                .map_err(|_dropped| "the serve VM is no longer running".to_owned())?;
            // One shape for both arms: the tool's reply, or the wedged
            // marker. The wait reads the same shared deadline the VM's
            // interrupt fires at; on expiry it drains WEDGE_GRACE for the
            // unwind (never a poison), then gives up naming the wedged
            // marker.
            wait_for_reply(&reply_rx, deadline)
        })
        .await
        .unwrap_or_else(|error| Err(format!("serve dispatch task failed: {error}")));

        match result {
            Ok(result_json) => {
                let structured: serde_json::Value =
                    serde_json::from_str(&result_json).map_err(|error| {
                        ErrorData::internal_error(
                            format!("the tool's result is not valid JSON: {error}"),
                            None,
                        )
                    })?;
                Ok(CallToolResult::structured(structured).into())
            }
            Err(message) => {
                let tool_name = tool_name_clone;
                if message == WEDGED_SENTINEL {
                    // No reply within the shared deadline plus the grace
                    // drain: answer the tool error, then refuse every
                    // later call.
                    tracing::warn!(tool = %tool_name, "luau tool call wedged past its budget; poisoning the server");
                    self.poisoned.store(true, Ordering::SeqCst);
                    return Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                        "{tool_name} failed: the call exceeded its per-call budget \
                         ({}); the tool may still be running server-side, and \
                         the server will refuse further calls — restart it \
                         (<c-r> in the MCP picker)",
                        self.budget
                            .secs
                            .map_or_else(|| "disabled".to_owned(), |s| format!("{s}s"))
                    ))])
                    .into());
                }
                tracing::warn!(tool = %tool_name, message = %message, "luau tool failed");
                Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                    "{tool_name} failed: {message}"
                ))])
                .into())
            }
        }
    }
}

impl ServerHandler for LuaServeServer {
    fn get_info(&self) -> InitializeResult {
        // InitializeResult is #[non_exhaustive]; construct via ::new.
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
    }

    // Trait-required async signatures; both arms are one-line
    // delegations to the context-free twins above.
    #[expect(clippy::unused_async_trait_impl, reason = "trait signature is async")]
    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(self.test_list_tools())
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        self.test_call_tool(request).await
    }
}

#[cfg(test)]
#[path = "serve_tests.rs"]
mod serve_tests;
