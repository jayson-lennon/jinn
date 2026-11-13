//! The rmcp server handler exposing smolvm's two tools.
//!
//! - `vm_run` — run a shell command in the guest via `bash -lc`, capture
//!   stdout/stderr/exit code; honors `timeout_secs` (default 60, cap 600)
//!   and `workdir` (default `/workspace`).
//! - `vm_info` — the machine manifest plus live guest facts.
//!
//! Failures are **tool-level** (`CallToolResult::error`): the model
//! submitted a runnable request and the machine rejected it, so the reason
//! belongs in content the model can read and react to. Protocol-level
//! errors (`ErrorData`) are reserved for requests that cannot be routed at
//! all — an unknown tool name, or arguments that cannot be interpreted as
//! the tool's input.

use std::sync::Arc;

use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ErrorData,
    InitializeResult, ListToolsResult, PaginatedRequestParams, ServerCapabilities, Tool,
};
use serde_json::{Value, json};

use crate::backend::{ExecOptions, SmolvmBackend, TIMEOUT_EXIT_CODE_LABEL};
use crate::tools;

/// The smolvm MCP server: a booted machine behind the backend seam, plus
/// the manifest that booted it.
pub struct SmolvmMcpServer {
    backend: Arc<dyn SmolvmBackend>,
    /// The manifest path, for `vm_info`'s text.
    manifest_path: String,
    /// The manifest contents verbatim, for `vm_info`'s text.
    manifest_contents: String,
}

/// Converts a JSON schema literal into rmcp's `JsonObject` input-schema form.
///
/// Panics only on a non-object literal in this file, which would be a bug in
/// the literal itself; `from_value` keeps that failure out of runtime data.
fn to_schema(value: serde_json::Value) -> rmcp::model::JsonObject {
    match serde_json::from_value(value) {
        Ok(object) => object,
        Err(err) => {
            tracing::error!(%err, "static tool schema literal is not a JSON object");
            rmcp::model::JsonObject::new()
        }
    }
}

impl SmolvmMcpServer {
    /// Wraps the booted machine and the manifest that booted it.
    pub fn new(
        backend: Arc<dyn SmolvmBackend>,
        manifest_path: String,
        manifest_contents: String,
    ) -> Self {
        Self {
            backend,
            manifest_path,
            manifest_contents,
        }
    }

    fn vm_run_tool() -> Tool {
        Tool::new(
            "vm_run",
            "Run a shell command inside the session's sandboxed Linux microVM \
             (Alpine Linux guest) and capture the result. The guest has bash, \
             git, and python3 installed; the session's working directory is \
             mounted READ-WRITE at /workspace, which is also the default \
             working directory — file changes through this tool are real \
             changes to the session's tree. Commands run inside a VM, not on \
             the host: the host's processes, filesystem (outside the mount), \
             and network identity are not reachable. Use absolute guest paths \
             (they start at / in the VM). Long commands are killed at the \
             timeout (default 60s, max 600s) and return whatever output was \
             produced by then, with exit code 124.",
            Arc::new(to_schema(json!({
                "type": "object",
                "properties": {
                    "cmd": {
                        "type": "string",
                        "description": "The shell command to run, executed as `bash -lc <cmd>` \
                                       inside the guest."
                    },
                    "timeout_secs": {
                        "type": "integer",
                        "description": "Seconds before the command is killed (1-600; default 60). \
                                       On timeout the command is terminated and whatever stdout/stderr \
                                       was produced comes back with exit code 124."
                    },
                    "workdir": {
                        "type": "string",
                        "description": "Absolute working directory inside the guest (default /workspace)."
                    }
                },
                "required": ["cmd"]
            }))),
        )
    }

    fn vm_info_tool() -> Tool {
        Tool::new(
            "vm_info",
            "Report the session VM's configuration and live state: the machine \
             manifest that booted it (path + verbatim contents), plus live \
             guest facts — distro, kernel, CPUs, memory, the /workspace mount, \
             and uptime.",
            Arc::new(to_schema(json!({
                "type": "object",
                "properties": {}
            }))),
        )
    }

    /// Renders one tool-level failure: error content the model can read.
    fn tool_error(tool: &'static str, message: &str) -> CallToolResponse {
        tracing::warn!(tool, message, "smolvm tool failed");
        CallToolResult::error(vec![ContentBlock::text(format!(
            "{tool} failed: {message}"
        ))])
        .into()
    }

    /// The raw arguments map of a tool call.
    fn args(request: &CallToolRequestParams) -> Result<&serde_json::Map<String, Value>, ErrorData> {
        request
            .arguments
            .as_ref()
            .ok_or_else(|| ErrorData::invalid_params("missing tool arguments", None))
    }

    /// Reads an optional integer argument, mapping a wrong-typed value to a
    /// protocol error (the request itself is uninterpretable).
    fn optional_u64(
        args: &serde_json::Map<String, Value>,
        field: &str,
    ) -> Result<Option<u64>, ErrorData> {
        match args.get(field) {
            None | Some(Value::Null) => Ok(None),
            Some(value) => value.as_u64().map(Some).ok_or_else(|| {
                ErrorData::invalid_params(
                    format!("`{field}` must be an integer, got {value}"),
                    None,
                )
            }),
        }
    }

    /// `vm_run`: validate, wrap in `bash -lc`, execute, render.
    fn run_vm_run(
        &self,
        args: &serde_json::Map<String, Value>,
    ) -> Result<CallToolResponse, ErrorData> {
        let cmd = args.get("cmd").and_then(Value::as_str).ok_or_else(|| {
            ErrorData::invalid_params("`cmd` is required and must be a string", None)
        })?;
        let timeout_secs = Self::optional_u64(args, "timeout_secs")?;
        let workdir = args.get("workdir").and_then(Value::as_str);

        let outcome = match tools::run_command(self.backend.as_ref(), cmd, timeout_secs, workdir) {
            Ok(outcome) => outcome,
            // Validation and guest failures are tool-level: the model can
            // correct the arguments and retry.
            Err(failure) => {
                return Ok(Self::tool_error(
                    "vm_run",
                    &tools::run_failure_message(&failure),
                ));
            }
        };

        // A timeout that killed the guest command is annotated on stderr so
        // the model knows the output is partial and why.
        let stderr = if outcome.exit_code == TIMEOUT_EXIT_CODE_LABEL {
            format!(
                "{}\n[vm_run: the command was killed after its timeout; output above is partial]",
                outcome.stderr
            )
        } else {
            outcome.stderr.clone()
        };

        Ok(CallToolResult::success(vec![ContentBlock::text(format!(
            "exit_code: {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            outcome.exit_code, outcome.stdout, stderr
        ))])
        .into())
    }

    /// `vm_info`: one guest probe, plus the manifest block.
    fn run_vm_info(&self) -> CallToolResponse {
        let facts = self
            .backend
            .exec(
                vec![
                    "sh".to_owned(),
                    "-c".to_owned(),
                    tools::GUEST_FACTS_SCRIPT.to_owned(),
                ],
                ExecOptions {
                    workdir: Some(tools::DEFAULT_WORKDIR.to_owned()),
                    timeout: Some(std::time::Duration::from_secs(tools::DEFAULT_TIMEOUT_SECS)),
                },
            )
            .ok()
            .map(|outcome| outcome.stdout);

        CallToolResult::success(vec![ContentBlock::text(tools::info_text(
            facts.as_deref(),
            self.backend.as_ref(),
            &self.manifest_path,
            &self.manifest_contents,
        ))])
        .into()
    }
}

impl ServerHandler for SmolvmMcpServer {
    fn get_info(&self) -> InitializeResult {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
    }

    // Trait-required async signature; listing is static and never awaits.
    #[expect(clippy::unused_async_trait_impl, reason = "trait signature is async")]
    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(vec![
            Self::vm_run_tool(),
            Self::vm_info_tool(),
        ]))
    }

    // Trait-required async signature; the guest execs run on the blocking
    // pool through the backend, never via .await here.
    #[expect(clippy::unused_async_trait_impl, reason = "trait signature is async")]
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        // Unknown tools cannot be routed at all — a protocol error, not a
        // tool-level one.
        match request.name.as_ref() {
            "vm_run" => self.run_vm_run(Self::args(&request)?),
            "vm_info" => Ok(self.run_vm_info()),
            other => Err(ErrorData::invalid_params(
                format!("unknown tool `{other}`; expected one of: vm_run, vm_info"),
                None,
            )),
        }
    }
}
