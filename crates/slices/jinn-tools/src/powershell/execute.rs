//! PowerShell execution half — spawn, stream, tear down.
//!
//! Everything that runs a real PowerShell process lives here: spawning
//! with terminal isolation, streaming stdout and stderr line by line,
//! the kill-on-drop guard that terminates the process tree when the
//! tool's future is aborted, and the exit-status formatting that
//! produces the final [`ToolResult`]. Compiled only on Windows, where
//! the tool is registered; the schema in the parent module is compiled
//! on every platform.

use std::process::Stdio;

use jinn_core_types::tool_types::{ToolCall, ToolResult};
use jinn_kernel::common::process_kill::kill_process_tree;
use jinn_tools_msg::ToolExecutionStarted;
use jinn_tools_msg::truncation::{
    DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, format_size, truncate_tail,
};
use marked_path::{Absolute, MarkedPathAccess, MarkedPathBuf};
use tokio::sync::mpsc;

use crate::BoxedToolFuture;
use crate::tool_types::{ToolContext, failed};

/// RAII guard that kills the entire process tree when dropped.
///
/// The same guard the `bash` tool uses: aborting the tool's future (on a
/// timeout, or on a cancelled turn) drops the handle, and the drop
/// terminates the child and its descendants rather than orphaning them.
struct KillOnDrop {
    child: tokio::process::Child,
}

impl KillOnDrop {
    /// Wraps a spawned child process in the kill-on-drop guard.
    fn new(child: tokio::process::Child) -> Self {
        Self { child }
    }
}

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        // The cross-platform tree-kill helper: on Unix it signals the
        // child's whole process group, on Windows it enumerates and
        // terminates the process tree.
        kill_process_tree(&mut self.child);
    }
}

impl std::ops::Deref for KillOnDrop {
    type Target = tokio::process::Child;
    fn deref(&self) -> &Self::Target {
        &self.child
    }
}

impl std::ops::DerefMut for KillOnDrop {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.child
    }
}

/// Parses the command from the tool call JSON.
///
/// Only extracts `command` — the `max_duration_secs` override is peeked
/// by the dispatcher's `extract_max_duration` reserved-field helper.
fn parse_args(raw: &str) -> Result<String, serde_json::Error> {
    let v: serde_json::Value = serde_json::from_str(raw)?;
    let command = v
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_owned();
    Ok(command)
}

/// Spawns a PowerShell command with stdout/stderr piped and terminal-isolated.
fn spawn_powershell_command(
    command: &str,
    cwd: &MarkedPathBuf<Absolute>,
    powershell: &str,
) -> std::io::Result<KillOnDrop> {
    let mut std_cmd = std::process::Command::new(powershell);
    std_cmd
        .args(jinn_common::shell::powershell_command_args(command))
        .current_dir(cwd.as_path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // Detach from jinn's console (CREATE_NO_WINDOW) so a console write
    // cannot print over the TUI, and so the child has no controlling
    // terminal to inherit.
    jinn_common::process_isolation::isolate(&mut std_cmd);

    let mut cmd = tokio::process::Command::from(std_cmd);
    cmd.spawn().map(KillOnDrop::new)
}

/// Reads lines from an async buffered reader and sends them through an mpsc channel.
fn spawn_line_reader<R>(reader: R, tx: mpsc::UnboundedSender<String>) -> tokio::task::JoinHandle<()>
where
    R: tokio::io::AsyncBufRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        use tokio::io::AsyncBufReadExt;
        let mut lines = reader.lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if tx.send(line).is_err() {
                break;
            }
        }
    })
}

/// Emits a streaming tool event if both bus and session_id are available.
async fn emit_stream_event(
    bus: Option<&jinn_kernel::common::services::bus_service::BusService>,
    session_id: Option<&jinn_core_types::SessionId>,
    event: impl jinn_kernel::common::bus::BusMessage
    + trouper::schema::Schema
    + serde::Serialize
    + trouper::envelope::PayloadValue,
) {
    if let (Some(bus), Some(_)) = (bus, session_id) {
        bus.publish(event).await;
    }
}

/// Formats the final [`ToolResult`] from the process exit status and accumulated output.
fn format_exit_result(
    exit_result: &Result<std::process::ExitStatus, std::io::Error>,
    accumulated: &str,
    tool_call_id: jinn_core_types::ToolCallId,
    tool_name: String,
    max_lines: usize,
    max_bytes: usize,
) -> ToolResult {
    let mut content = accumulated.to_owned();
    let success = match exit_result {
        Ok(status) => status.success(),
        Err(_) => false,
    };

    if !success && !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    if !success {
        let code = exit_result
            .as_ref()
            .ok()
            .and_then(std::process::ExitStatus::code)
            .map_or_else(|| "unknown (signal)".to_owned(), |code| code.to_string());
        content.push_str(&format!("Command exited with code {code}"));
    }

    let truncation_result = truncate_tail(&content, max_lines, max_bytes);
    if truncation_result.truncated {
        let meta = truncation_result.meta;
        let full = Some(content);
        let truncated = truncation_result.content;
        let (output, truncation) = match meta {
            Some(meta) => {
                let start_line = meta.total_lines.saturating_sub(meta.output_lines) + 1;
                let end_line = meta.total_lines;
                let notice = if meta.truncated_by == jinn_core_types::tool_types::TruncatedBy::Bytes
                {
                    format!(
                        "\n\n[Showing lines {start_line}-{end_line} of {} ({} limit)]",
                        meta.total_lines,
                        format_size(max_bytes)
                    )
                } else {
                    format!(
                        "\n\n[Showing lines {start_line}-{end_line} of {}]",
                        meta.total_lines
                    )
                };
                let mut output = truncated;
                output.push_str(&notice);
                (output, Some(meta))
            }
            None => (truncated, None),
        };
        ToolResult {
            tool_call_id,
            name: tool_name,
            content: output,
            success,
            full_content: full,
            truncation,
            pin_position: None,
            attachments: Vec::new(),
        }
    } else {
        ToolResult {
            tool_call_id,
            name: tool_name,
            content: truncation_result.content,
            success,
            full_content: None,
            truncation: None,
            pin_position: None,
            attachments: Vec::new(),
        }
    }
}

/// Executes the `powershell` built-in tool.
///
/// Windows-only: the tool is not registered elsewhere, so this is never
/// reached on Linux or macOS.
pub fn execute(call: ToolCall, ctx: ToolContext) -> BoxedToolFuture {
    Box::pin(async move {
        let command = match parse_args(&call.arguments) {
            Ok(command) => command,
            Err(error) => {
                return failed(
                    call.id,
                    call.name,
                    format!("failed to parse arguments: {error}"),
                );
            }
        };

        if command.is_empty() {
            return failed(call.id, call.name, "command is empty".to_owned());
        }

        // The PowerShell executable, resolved at launch. A machine with no
        // PowerShell is a real state, and the model reads why the command
        // could not run.
        let powershell = match ctx.shells.read().powershell() {
            Ok(shell) => shell.program().to_owned(),
            Err(error) => {
                return failed(call.id, call.name, error.to_string());
            }
        };

        let cwd = ctx.cwd.clone();

        emit_stream_event(
            ctx.bus.as_ref(),
            ctx.session_id.as_ref(),
            ToolExecutionStarted {
                session_id: ctx.session_id.clone().unwrap_or_default(),
                turn: ctx.turn,
                tool_call_id: call.id.clone(),
                name: call.name.clone(),
                dispatched_at: jiff::Timestamp::now(),
            },
        )
        .await;

        let mut child = match spawn_powershell_command(&command, &cwd, &powershell) {
            Ok(child) => child,
            Err(error) => {
                return failed(
                    call.id,
                    call.name,
                    format!("failed to execute command: {error}"),
                );
            }
        };

        let Some(stdout) = child.stdout.take() else {
            return failed(
                call.id,
                call.name,
                "failed to capture stdout: pipe was not set up".to_owned(),
            );
        };
        let Some(stderr) = child.stderr.take() else {
            return failed(
                call.id,
                call.name,
                "failed to capture stderr: pipe was not set up".to_owned(),
            );
        };

        let max_lines = ctx.max_output_lines.unwrap_or(DEFAULT_MAX_LINES);
        let max_bytes = ctx.max_output_bytes.unwrap_or(DEFAULT_MAX_BYTES);

        let mut accumulated = String::new();
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let stdout_handle = spawn_line_reader(tokio::io::BufReader::new(stdout), tx.clone());
        let stderr_handle = spawn_line_reader(tokio::io::BufReader::new(stderr), tx);

        while let Some(line) = rx.recv().await {
            accumulated.push_str(&line);
            accumulated.push('\n');
        }

        let _ = stdout_handle.await;
        let _ = stderr_handle.await;

        let exit_result = child.wait().await;

        format_exit_result(
            &exit_result,
            &accumulated,
            call.id,
            call.name,
            max_lines,
            max_bytes,
        )
    })
}
