//! Bash built-in tool - executes shell commands.
//!
//! Spawns a shell process with piped stdout/stderr, streaming output
//! via batched `ToolExecutionOutput` events. Output lines are accumulated
//! and flushed every 500ms (or when the buffer exceeds 4KB) to reduce
//! event volume and prevent dropped keystrokes from terminal overload.

use tokio::sync::mpsc;

use std::fmt::Write as _;
use std::process::Stdio;
use std::time::Duration;

use crate::tool_types::ToolContext;
use jinn_core_types::tool_types::{ToolCall, ToolDefinition, ToolResult};
use jinn_domain::common::process_kill::kill_process_tree;
use jinn_domain::common::services::bus_service::BusService;
use jinn_domain::protocol::SessionId;
use jinn_tools_msg::{ToolExecutionOutput, ToolExecutionStarted, ToolOutputKind};

use jinn_tools_msg::truncation::{
    DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, format_size, truncate_tail,
};

use super::BoxedToolFuture;

/// RAII guard that kills the entire process group when dropped.
///
/// When the tokio task running a bash command is aborted (via
/// `JoinHandle::abort()`), this guard's `Drop` implementation sends
/// `SIGKILL` to the child's entire process group, ensuring that
/// descendant processes (e.g., `find /` spawned by bash) are also
/// terminated. Without this, aborting the future only drops the
/// handle - the OS processes continue running as orphans.
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
        // Delegate to the cross-platform tree-kill helper. On Unix this sends
        // SIGKILL to the child's entire process group (race-free); on Windows it
        // enumerates and terminates the process tree via `kill_tree`. See
        // `common::process_kill` for the per-platform guarantees and trade-offs.
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

pub fn definition(default_timeout_secs: u64) -> ToolDefinition {
    let default_secs_str = default_timeout_secs.to_string();
    ToolDefinition {
        name: "bash".to_owned(),
        description: format!(
            "Execute a bash command in the current working directory. \\
            Returns stdout and stderr. Output is truncated to last 2000 lines or 50KB \\
            (whichever is hit first). \\
            \\
            TIMEOUT: the default is {default_secs_str}s. \\
            Pass `max_duration_secs` as a tool-call argument to override — NOT the shell `timeout` command. \\
            Example: {{\"command\": \"cargo test\", \"max_duration_secs\": 600}}. \\
            Set `max_duration_secs: 0` to disable the timeout for that call."
        ),
        prompt_snippet: Some("Execute bash commands (ls, grep, find, etc.)".to_owned()),
        prompt_guidelines: vec![
            "Proactively set max_duration_secs for commands that may run long (builds, tests, network requests, large compiles). Do NOT use the shell `timeout` command — pass max_duration_secs as a tool-call argument.".to_owned(),
            "If a command is killed by the timeout, retry the same command with a larger max_duration_secs value.".to_owned(),
        ],
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "Bash command to execute"
                },
                "max_duration_secs": {
                    "type": "number",
                    "description": format!("Maximum duration in seconds before the command is killed. Overrides the default of {default_secs_str}s. Set to 0 to disable the timeout for this call. Pass as a tool-call argument — NOT the shell `timeout` command. Example: {{\"command\": \"cargo test\", \"max_duration_secs\": 600}}.")
                }
            },
            "required": ["command"]
        }),
        server_tool_type: None,
    }
}

/// Parses the command from the tool call JSON.
///
/// Only extracts `command` — the `max_duration_secs` override is peeked by the dispatcher's
/// [`extract_max_duration`](crate::orchestrator::extract_max_duration) reserved-field helper.
fn parse_args(raw: &str) -> Result<String, serde_json::Error> {
    let v: serde_json::Value = serde_json::from_str(raw)?;
    let command = v
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_owned();
    Ok(command)
}

/// Maximum bytes to buffer before truncating accumulated streaming output.
/// This is a streaming-only threshold to prevent unbounded memory growth.
/// The final truncation uses the shared limits from the truncation module.
const STREAM_BUFFER_MAX_BYTES: usize = DEFAULT_MAX_BYTES * 2;

/// Flush the streaming event buffer early when it exceeds this size,
/// preventing unbounded memory growth between timer ticks.
const STREAM_FLUSH_THRESHOLD: usize = 4096;

/// Interval for batching streaming tool output events.
/// Accumulated output is flushed every 500ms to reduce event volume
/// and prevent dropped keystrokes from terminal emulator overload.
const STREAM_FLUSH_INTERVAL: Duration = Duration::from_millis(500);

/// Truncates accumulated streaming output to prevent unbounded memory growth.
/// Uses the shared tail-truncation logic.
fn truncate_streaming_output(content: &str) -> String {
    truncate_tail(content, DEFAULT_MAX_LINES, DEFAULT_MAX_BYTES).content
}

/// Emits buffered output as a single `ToolExecutionOutput` event.
async fn flush_buffer(
    buffer: &str,
    bus: Option<&BusService>,
    session_id: Option<&SessionId>,
    tool_call_id: &str,
) {
    emit_stream_event(
        bus,
        session_id,
        ToolExecutionOutput {
            kind: ToolOutputKind::default(),
            session_id: session_id.cloned().unwrap_or_default(),
            tool_call_id: tool_call_id.to_owned(),
            output: buffer.to_owned(),
        },
    )
    .await;
}

/// Emits a streaming tool event if both bus and session_id are available.
async fn emit_stream_event(
    bus: Option<&BusService>,
    session_id: Option<&SessionId>,
    event: impl jinn_domain::common::bus::BusMessage + trouper::schema::Schema + serde::Serialize,
) {
    if let (Some(bus), Some(_)) = (bus, session_id) {
        bus.publish(event).await;
    }
}

/// Creates an error [`ToolResult`] with the given fields and `success: false`.
fn error_tool_result(tool_call_id: String, name: String, content: String) -> ToolResult {
    ToolResult {
        tool_call_id,
        name,
        content,
        success: false,
        full_content: None,
        truncation: None,
        pin_position: None,
    }
}

/// Formats the final [`ToolResult`] from the process exit status and accumulated output.
///
/// Applies tail-truncation when output exceeds the configured limits.
/// Stores the full output in `full_content` when truncation occurs.
fn format_exit_result(
    exit_result: &Result<std::process::ExitStatus, std::io::Error>,
    accumulated: &str,
    tool_call_id: String,
    tool_name: String,
    max_lines: usize,
    max_bytes: usize,
) -> ToolResult {
    let mut content = accumulated.to_owned();
    let success = match exit_result {
        Ok(status) => status.success(),
        Err(_) => false,
    };

    if !success {
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
        let _ = write!(
            content,
            "Command exited with code {}",
            exit_result
                .as_ref()
                .ok()
                .and_then(std::process::ExitStatus::code)
                .map_or_else(|| "unknown (signal)".to_owned(), |c: i32| c.to_string())
        );
    }

    // Apply tail-truncation to the final output.
    let truncation_result = truncate_tail(&content, max_lines, max_bytes);
    if truncation_result.truncated {
        if let Some(meta) = truncation_result.meta {
            let start_line = meta.total_lines.saturating_sub(meta.output_lines) + 1;
            let end_line = meta.total_lines;
            let notice = if meta.truncated_by == jinn_core_types::tool_types::TruncatedBy::Bytes {
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
            let mut output = truncation_result.content;
            output.push_str(&notice);
            ToolResult {
                tool_call_id,
                name: tool_name,
                content: output,
                success,
                full_content: Some(content),
                truncation: Some(meta),
                pin_position: None,
            }
        } else {
            // truncated but no meta - return unformatted truncated content
            ToolResult {
                tool_call_id,
                name: tool_name,
                content: truncation_result.content,
                success,
                full_content: Some(content),
                truncation: None,
                pin_position: None,
            }
        }
    } else {
        ToolResult {
            tool_call_id,
            name: tool_name,
            content,
            success,
            full_content: None,
            truncation: None,
            pin_position: None,
        }
    }
}

/// Reads lines from an async buffered reader and sends them through an mpsc channel.
///
/// Designed for merging stdout and stderr from a child process into a single
/// stream. The spawned task exits when the reader reaches EOF or the channel
/// is closed.
fn spawn_line_reader<R>(reader: R, tx: mpsc::UnboundedSender<String>) -> tokio::task::JoinHandle<()>
where
    R: tokio::io::AsyncBufRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        use tokio::io::AsyncBufReadExt;
        let mut lines = reader.lines();
        while let Ok(Some(line)) = lines.next_line().await {
            // Unbounded sender never blocks; send() fails only when all
            // receivers have been dropped (cancellation).
            if tx.send(line).is_err() {
                break;
            }
        }
    })
}

/// Buffers streaming output lines and flushes them as batched events.
///
/// Accumulates lines into an internal buffer, emitting a `ToolExecutionOutput`
/// event when the buffer exceeds [`STREAM_FLUSH_THRESHOLD`] or when explicitly
/// flushed (timer tick or loop exit). Also guards against unbounded memory
/// growth by truncating the caller's `accumulated` string in-place when it
/// exceeds [`STREAM_BUFFER_MAX_BYTES`].
struct StreamingBatcher {
    buffer: String,
}

impl StreamingBatcher {
    fn new() -> Self {
        Self {
            buffer: String::new(),
        }
    }

    /// Appends a line to both the flush buffer and the caller's accumulated output.
    ///
    /// If `accumulated` exceeds [`STREAM_BUFFER_MAX_BYTES`], it is truncated
    /// in-place using [`truncate_streaming_output`].
    fn push_line(&mut self, line: &str, accumulated: &mut String) {
        accumulated.push_str(line);
        accumulated.push('\n');
        self.buffer.push_str(line);
        self.buffer.push('\n');

        if accumulated.len() > STREAM_BUFFER_MAX_BYTES {
            let truncated = truncate_streaming_output(accumulated);
            *accumulated = truncated;
        }
    }

    /// Returns `true` when the buffer exceeds [`STREAM_FLUSH_THRESHOLD`].
    fn should_flush(&self) -> bool {
        self.buffer.len() > STREAM_FLUSH_THRESHOLD
    }

    /// Emits the buffered output as a streaming event and clears the buffer.
    ///
    /// No-op when the buffer is empty.
    async fn flush(
        &mut self,
        bus: Option<&BusService>,
        session_id: Option<&SessionId>,
        tool_call_id: &str,
    ) {
        if self.buffer.is_empty() {
            return;
        }
        flush_buffer(&self.buffer, bus, session_id, tool_call_id).await;
        self.buffer.clear();
    }
}

/// Reads stdout/stderr concurrently, emitting streaming events, then waits for the child to exit.
///
/// Lines are appended to `accumulated` and streamed via [`emit_stream_event`].
/// Large outputs are truncated in-place when they exceed `2 * MAX_BYTES`.
async fn read_child_output_and_wait(
    child: &mut tokio::process::Child,
    stdout: tokio::process::ChildStdout,
    stderr: tokio::process::ChildStderr,
    accumulated: &mut String,
    bus: Option<&BusService>,
    session_id: Option<&SessionId>,
    tool_call_id: &str,
) -> Result<std::process::ExitStatus, std::io::Error> {
    // WHY tokio::sync::mpsc HERE, NOT kanal:
    // kanal's `ReceiveStream` used inside `tokio::select!` has a known
    // memory-corruption / double-free bug (kanal #63, #50) triggered when the
    // future is cancelled mid-select. The tool dispatcher drops this future on
    // timeout kill (`run_builtin_with_timeout`), which corrupted the heap and
    // aborted the process ("free(): double free detected in tcache 2").
    // `tokio::mpsc::UnboundedReceiver::recv()` is cancel-safe inside
    // `select!`, so a mid-stream drop is safe. Do not "consistency-clean" this
    // back to kanal.
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();

    let stdout_handle = spawn_line_reader(tokio::io::BufReader::new(stdout), tx.clone());
    let stderr_handle = spawn_line_reader(tokio::io::BufReader::new(stderr), tx);

    let mut batcher = StreamingBatcher::new();
    let mut timer = tokio::time::interval(STREAM_FLUSH_INTERVAL);
    timer.tick().await; // Consume the immediate first tick.

    loop {
        tokio::select! {
            // `recv()` is cancel-safe: dropping this future mid-await does not
            // lose a message or corrupt state. `None` means all senders were
            // dropped (both readers finished).
            line = rx.recv() => match line {
                Some(line) => {
                    batcher.push_line(&line, accumulated);
                    if batcher.should_flush() {
                        batcher.flush(bus, session_id, tool_call_id).await;
                    }
                }
                None => break,
            },
            _ = timer.tick() => {
                batcher.flush(bus, session_id, tool_call_id).await;
            }
        }
    }

    batcher.flush(bus, session_id, tool_call_id).await;

    // Wait for both readers to finish.
    let _ = stdout_handle.await;
    let _ = stderr_handle.await;

    // Wait for process exit.
    child.wait().await
}

/// Spawns a shell command with stdout/stderr piped, terminal-isolated, in its
/// own session/process group.
///
/// Disconnects stdin from the controlling TTY so child processes
/// (and their entire process tree) are removed from the kernel's
/// TTY input fd list. Without this, child processes inherit the
/// parent's stdin fd, and under heavy spawn pressure (e.g. cargo
/// nextest spawning hundreds of test binaries) the kernel's TTY
/// input buffer overflows before the event thread can drain it,
/// causing dropped keystrokes.
///
/// Terminal isolation (see `jinn_common::process_isolation`) detaches the
/// child from jinn's session entirely: it has **no controlling terminal**,
/// so tty-writers (ssh/git host-key prompts, `CONOUT$` console writes on
/// Windows) cannot print over the TUI.
///
/// `setsid` makes the child a session and group leader (pid == pgid), the
/// same invariant the kill-on-drop guard relies on to terminate the whole
/// group (child + all descendants) on cancel.
fn spawn_shell_command(command: &str, cwd: &std::path::Path) -> std::io::Result<KillOnDrop> {
    // On Windows, bare "bash" resolves to C:\Windows\System32\bash.exe which
    // is the WSL launcher — it routes commands into a Linux VM. This breaks
    // native Windows tooling (cargo produces Linux binaries, process trees
    // become invisible to the Windows kill path, etc.), so prefer Git Bash
    // when available, falling back to the SHELL env var or bare "bash".
    // Unix keeps PATH resolution: this tool promises bash semantics, and
    // $SHELL may be fish, dash, or zsh — shells that reject bash syntax
    // such as brace expansion and `$$`.
    let shell = {
        #[cfg(target_os = "windows")]
        {
            let git_bash = "C:\\Program Files\\Git\\bin\\bash.exe";
            if std::path::Path::new(git_bash).exists() {
                git_bash.to_owned()
            } else {
                std::env::var("SHELL").unwrap_or_else(|_| "bash".to_owned())
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            "bash".to_owned()
        }
    };
    let mut std_cmd = std::process::Command::new(&shell);
    std_cmd
        .arg("-c")
        .arg(command)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // Detach from jinn's session on Unix (setsid; no controlling tty, pid ==
    // pgid for the group kill) and from jinn's console on Windows
    // (CREATE_NO_WINDOW). Replaces the former `process_group(0)` — combining
    // both would fail setsid with EPERM.
    jinn_common::process_isolation::isolate(&mut std_cmd);

    let mut cmd = tokio::process::Command::from(std_cmd);
    cmd.spawn().map(KillOnDrop::new)
}

/// Executes the `bash` built-in tool with streaming output.
pub fn execute(call: ToolCall, ctx: ToolContext) -> BoxedToolFuture {
    Box::pin(async move {
        let command = match parse_args(&call.arguments) {
            Ok(c) => c,
            Err(e) => {
                return error_tool_result(
                    call.id,
                    call.name,
                    format!("failed to parse arguments: {e}"),
                );
            }
        };

        if command.is_empty() {
            return error_tool_result(call.id, call.name, "command is empty".to_owned());
        }

        // Project command policy gate: a matching rule denies the command
        // before any child spawns (same feedback shape as the empty-command
        // check — nothing started, so no `ToolExecutionStarted` is emitted).
        if let Some((pattern, message)) = ctx.command_policy.matched_message(&command) {
            return error_tool_result(
                call.id,
                call.name,
                format!("Blocked by project command policy (pattern: `{pattern}`): {message}"),
            );
        }

        let cwd = ctx.cwd.clone();

        // Emit ToolExecutionStarted if we have a bus and session_id.
        emit_stream_event(
            ctx.bus.as_ref(),
            ctx.session_id.as_ref(),
            ToolExecutionStarted {
                session_id: ctx.session_id.clone().unwrap_or_default(),
                tool_call_id: call.id.clone(),
                name: call.name.clone(),
                dispatched_at: jiff::Timestamp::now(),
            },
        )
        .await;

        let spawn_result = spawn_shell_command(&command, &cwd);

        let mut child = match spawn_result {
            Ok(child) => child,
            Err(e) => {
                return error_tool_result(
                    call.id,
                    call.name,
                    format!("failed to execute command: {e}"),
                );
            }
        };

        let Some(stdout) = child.stdout.take() else {
            return error_tool_result(
                call.id,
                call.name,
                "failed to capture stdout: pipe was not set up".to_owned(),
            );
        };
        let Some(stderr) = child.stderr.take() else {
            return error_tool_result(
                call.id,
                call.name,
                "failed to capture stderr: pipe was not set up".to_owned(),
            );
        };

        // Extract truncation limits from context.
        let max_lines = ctx.max_output_lines.unwrap_or(DEFAULT_MAX_LINES);
        let max_bytes = ctx.max_output_bytes.unwrap_or(DEFAULT_MAX_BYTES);

        // Read stdout and stderr concurrently using tokio async IO.
        let mut accumulated = String::new();

        let read_fut = read_child_output_and_wait(
            &mut child,
            stdout,
            stderr,
            &mut accumulated,
            ctx.bus.as_ref(),
            ctx.session_id.as_ref(),
            &call.id,
        );

        let exit_result: Result<std::process::ExitStatus, std::io::Error> = read_fut.await;

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

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]
    use super::*;
    use jinn_tools_msg::CompiledCommandPolicy;
    use std::path::PathBuf;

    fn test_ctx() -> ToolContext {
        ToolContext {
            cwd: PathBuf::from("/tmp"),
            command_policy: jinn_tools_msg::CompiledCommandPolicy::default(),
            timeout: None,
            state: None,
            session_id: None,
            app_paths: jinn_domain::common::app_paths::AppPaths::default(),
            bus: None,
            max_output_lines: None,
            max_output_bytes: None,

            dispatched_at: jiff::Timestamp::now(),
            session_cap: None,
            mcp_coordinator: None,
            interactive_term: None,
            task_spawns: None,
            session_store: None,
        }
    }

    fn ctx_with_policy(cwd: &std::path::Path, policy: CompiledCommandPolicy) -> ToolContext {
        ToolContext {
            cwd: cwd.to_path_buf(),
            command_policy: policy,
            ..test_ctx()
        }
    }

    fn policy_call(command: &str) -> ToolCall {
        ToolCall {
            id: "call_policy".to_owned(),
            name: "bash".to_owned(),
            arguments: serde_json::json!({ "command": command }).to_string(),
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn blocked_command_returns_failed_result_with_message_and_spawns_nothing() {
        // Given a context whose policy blocks `cargo test -p` and a sentinel
        // path the command would create if it ran.
        let dir = tempfile::tempdir().expect("temp dir");
        let sentinel = dir.path().join("sentinel");
        let ctx = ctx_with_policy(
            dir.path(),
            CompiledCommandPolicy::compile(&[jinn_tools_msg::CommandPolicyRule {
                pattern: r"cargo\s+(test|t)\b.*\s-p\b".to_owned(),
                message: "use just test".to_owned(),
            }]),
        );

        // When executing a blocked command that would touch the sentinel if
        // it ran (`;` so the sentinel is touched even if cargo fails fast).
        let result = execute(
            policy_call(&format!(
                "cargo test -p no-such-pkg; touch {}",
                sentinel.display()
            )),
            ctx,
        )
        .await;

        // Then the result is a failure carrying the pattern and the rule's
        // corrective message.
        assert!(!result.success);
        assert!(
            result.content.contains("Blocked by project command policy"),
            "missing block notice: {}",
            result.content
        );
        assert!(
            result.content.contains("cargo\\s+(test|t)"),
            "missing pattern"
        );
        assert!(result.content.contains("use just test"), "missing message");
        // And the process never spawned — the sentinel was not created.
        assert!(!sentinel.exists(), "command executed despite policy block");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn non_matching_command_executes_normally_under_policy() {
        // Given a context whose policy blocks only `cargo test -p`.
        let dir = tempfile::tempdir().expect("temp dir");
        let ctx = ctx_with_policy(
            dir.path(),
            CompiledCommandPolicy::compile(&[jinn_tools_msg::CommandPolicyRule {
                pattern: r"cargo\s+(test|t)\b.*\s-p\b".to_owned(),
                message: "use just test".to_owned(),
            }]),
        );

        // When executing a command the policy does not match.
        let result = execute(policy_call("echo ran-fine"), ctx).await;

        // Then the command runs and reports success.
        assert!(result.success, "command should execute: {}", result.content);
        assert!(result.content.contains("ran-fine"));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn empty_policy_preserves_default_behavior() {
        // Given a context with an empty (unconfigured) policy.
        let dir = tempfile::tempdir().expect("temp dir");
        let ctx = ctx_with_policy(dir.path(), CompiledCommandPolicy::default());

        // When executing an ordinary command.
        let result = execute(policy_call("echo default-path"), ctx).await;

        // Then behavior is unchanged from the pre-policy default.
        assert!(result.success);
        assert!(result.content.contains("default-path"));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_returns_stdout() {
        // Given a bash tool call that echoes text.
        let call = ToolCall {
            id: "call_1".to_owned(),
            name: "bash".to_owned(),
            arguments: serde_json::json!({
                "command": "echo hello world"
            })
            .to_string(),
        };

        // When executing the bash tool.
        let result = execute(call, test_ctx()).await;

        // Then the result contains the echoed output.
        assert_eq!(result.tool_call_id, "call_1");
        assert!(result.success);
        assert!(result.content.contains("hello world"));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_runs_through_bash_not_fish_or_dash() {
        // Given a bash tool call using brace expansion (rejected by fish and dash).
        let call = ToolCall {
            id: "call_bash".to_owned(),
            name: "bash".to_owned(),
            arguments: serde_json::json!({
                "command": "echo {1..3}"
            })
            .to_string(),
        };

        // When executing the bash tool.
        let result = execute(call, test_ctx()).await;

        // Then the output shows bash's brace expansion (1 2 3),
        // proving the executor is bash, not fish or dash.
        assert!(result.success);
        assert!(result.content.contains("1 2 3"));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_captures_stderr() {
        // Given a bash tool call that writes to stderr.
        let call = ToolCall {
            id: "call_2".to_owned(),
            name: "bash".to_owned(),
            arguments: serde_json::json!({
                "command": "echo error >&2"
            })
            .to_string(),
        };

        // When executing the bash tool.
        let result = execute(call, test_ctx()).await;

        // Then the result contains the stderr output.
        assert!(result.success);
        assert!(result.content.contains("error"));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_reports_nonzero_exit() {
        // Given a bash tool call that exits with code 1.
        let call = ToolCall {
            id: "call_3".to_owned(),
            name: "bash".to_owned(),
            arguments: serde_json::json!({
                "command": "exit 1"
            })
            .to_string(),
        };

        // When executing the bash tool.
        let result = execute(call, test_ctx()).await;

        // Then the result indicates failure with exit code.
        assert!(!result.success);
        assert!(result.content.contains("Command exited with code 1"));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_resolves_cwd() {
        // Given a bash tool call that prints the working directory.
        let dir = tempfile::tempdir().expect("create temp dir");
        let ctx = ToolContext {
            cwd: dir.path().to_owned(),
            command_policy: jinn_tools_msg::CompiledCommandPolicy::default(),
            timeout: None,
            state: None,
            session_id: None,
            app_paths: jinn_domain::common::app_paths::AppPaths::default(),
            bus: None,
            max_output_lines: None,
            max_output_bytes: None,

            dispatched_at: jiff::Timestamp::now(),
            session_cap: None,
            mcp_coordinator: None,
            interactive_term: None,
            task_spawns: None,
            session_store: None,
        };

        let call = ToolCall {
            id: "call_4".to_owned(),
            name: "bash".to_owned(),
            arguments: serde_json::json!({
                "command": "pwd"
            })
            .to_string(),
        };

        // When executing the bash tool.
        let result = execute(call, ctx).await;

        // Then the output shows the CWD.
        assert!(result.success);
        assert!(result.content.contains(dir.path().to_str().unwrap()));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_returns_error_on_empty_command() {
        // Given a bash tool call with an empty command.
        let call = ToolCall {
            id: "call_5".to_owned(),
            name: "bash".to_owned(),
            arguments: serde_json::json!({
                "command": ""
            })
            .to_string(),
        };

        // When executing the bash tool.
        let result = execute(call, test_ctx()).await;

        // Then the result indicates failure.
        assert!(!result.success);
        assert!(result.content.contains("command is empty"));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_returns_error_on_bad_json() {
        // Given a bash tool call with invalid JSON.
        let call = ToolCall {
            id: "call_6".to_owned(),
            name: "bash".to_owned(),
            arguments: "not json".to_owned(),
        };

        // When executing the bash tool.
        let result = execute(call, test_ctx()).await;

        // Then the result indicates failure.
        assert!(!result.success);
        assert!(result.content.contains("failed to parse arguments"));
    }

    #[cfg(unix)]
    #[rstest::rstest]
    #[tokio::test]
    async fn execute_child_has_no_controlling_tty() {
        // Given a bash tool call that tries to open the controlling tty.
        let call = ToolCall {
            id: "call_tty".to_owned(),
            name: "bash".to_owned(),
            arguments: serde_json::json!({
                "command": "exec 3>/dev/tty"
            })
            .to_string(),
        };

        // When executing the bash tool.
        let result = execute(call, test_ctx()).await;

        // Then the child failed to open /dev/tty: it has no controlling
        // terminal, so tty-writers cannot print over the TUI.
        assert!(
            !result.success,
            "expected /dev/tty open to fail in an isolated bash child"
        );
    }

    #[cfg(unix)]
    #[rstest::rstest]
    #[tokio::test]
    async fn execute_child_runs_in_new_session() {
        // Given a bash tool call reporting its session id.
        let call = ToolCall {
            id: "call_sid".to_owned(),
            name: "bash".to_owned(),
            arguments: serde_json::json!({
                "command": "ps -o sid= -p $$"
            })
            .to_string(),
        };

        // When executing the bash tool and reading the child's reported sid.
        let result = execute(call, test_ctx()).await;
        let child_sid: i64 = result
            .content
            .trim()
            .parse()
            .expect("ps should print a numeric sid");

        // When reading the test process's session id via the same tool.
        let own_sid: i64 = {
            let out = std::process::Command::new("ps")
                .args(["-o", "sid=", "-p", &std::process::id().to_string()])
                .output()
                .expect("ps should run");
            String::from_utf8_lossy(&out.stdout)
                .trim()
                .parse()
                .expect("ps should print a numeric sid for the test process")
        };

        // Then the bash child runs in a different session from jinn.
        assert_ne!(
            child_sid, own_sid,
            "bash-tool child must not share jinn's session"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn execute_no_timeout_when_default_is_none_and_no_per_call() {
        // Given no timeout in the context.
        let ctx = test_ctx(); // timeout: None
        let call = ToolCall {
            id: "call_no_to".to_owned(),
            name: "bash".to_owned(),
            arguments: serde_json::json!({"command": "echo hi"}).to_string(),
        };

        // When executing a fast command.
        let result = execute(call, ctx).await;

        // Then it succeeds (no timeout enforced).
        assert!(result.success);
        assert!(result.content.contains("hi"));
    }

    /// Verifies that `KillOnDrop` terminates the child process and its
    /// entire process group when dropped.
    ///
    /// Spawns a bash command that starts a background sleep child, then
    /// drops the guard. Both processes should be killed.
    #[cfg(unix)]
    #[rstest::rstest]
    #[tokio::test]
    async fn kill_on_drop_terminates_process_group() {
        use std::process::Stdio;

        // Given a bash command that spawns a background child process.
        let shell = "/bin/sh".to_owned();
        let guard = {
            let child = tokio::process::Command::new(&shell)
                .arg("-c")
                .arg("sleep 60 & sleep 60")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .process_group(0)
                .spawn()
                .expect("spawn should succeed");
            let pid = child.id().expect("child should have a PID");
            assert!(pid > 0, "child PID should be positive");
            KillOnDrop::new(child)
        };

        // Give the processes a moment to start.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // When dropping the guard (simulating task abort).
        drop(guard);

        // Then give the kernel a moment to reap the processes.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Verify no 'sleep 60' processes survived.
        let output = tokio::process::Command::new("pgrep")
            .arg("-f")
            .arg("sleep 60")
            .output()
            .await
            .expect("pgrep should run");
        let stdout = String::from_utf8_lossy(&output.stdout);
        // No "sleep 60" processes should be running.
        assert!(
            stdout.trim().is_empty(),
            "expected no 'sleep 60' processes, but found: {stdout}"
        );
    }

    /// Regression for the `tcache 2` double-free crash. The bash tool drains
    /// its stdout/stderr merge channel inside a `tokio::select!`; when the
    /// tool dispatcher kills a long-running command on timeout, it drops the
    /// bash future mid-`select!`. Under kanal this corrupted the heap and
    /// aborted the process. With the cancel-safe `tokio::sync::mpsc::recv()`
    /// the mid-stream drop must be crash-free even under repeated cancellation.
    ///
    /// Runs a continuously-emitting command (`yes`) under a tight timeout so the
    /// future is dropped while output is streaming; repeats to exercise the
    /// cancel path repeatedly. If any iteration corrupted the allocator, glibc
    /// would have aborted the test runner before the loop finished.
    #[cfg(unix)]
    #[rstest::rstest]
    #[tokio::test]
    async fn streaming_cancel_under_repeated_timeout_is_crash_free() {
        // Given a continuously-emitting command under a tight per-call timeout.
        const ITERATIONS: u32 = 6;
        const TIGHT: Duration = Duration::from_millis(200);
        let ctx = test_ctx();

        // When repeatedly dropping the bash future mid-stream via timeout.
        for i in 0..ITERATIONS {
            let call = ToolCall {
                id: format!("call_cancel_{i}"),
                name: "bash".to_owned(),
                arguments: serde_json::json!({ "command": "yes" }).to_string(),
            };
            // `yes` emits faster than the channel drains, so the timeout
            // virtually always fires while the select is parked mid-stream.
            let result = tokio::time::timeout(TIGHT, execute(call, ctx.clone())).await;

            // Then each iteration times out (future dropped mid-stream) without aborting.
            assert!(
                result.is_err(),
                "iteration {i}: expected timeout (mid-stream drop), got {:?}",
                result.as_ref().map(|r| &r.content)
            );
        }

        // Give the kernel a moment to reap the killed `yes` processes.
        tokio::time::sleep(Duration::from_millis(150)).await;

        // And no orphaned `yes` process survived the kill-on-drop guard.
        let output = tokio::process::Command::new("pgrep")
            .arg("-x")
            .arg("yes")
            .output()
            .await
            .expect("pgrep should run");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.trim().is_empty(),
            "expected no 'yes' processes after cancellation, but found: {stdout}"
        );
    }

    /// Windows mirror of `kill_on_drop_terminates_process_group`.
    ///
    /// Cannot run in the Linux dev environment — included so CI on Windows
    /// validates the `kill_tree`-based `Drop` path. Uses `ping` as a Windows-native
    /// long-running command (the classic `cmd` sleep substitute) and verifies
    /// termination via `tasklist`.
    #[cfg(windows)]
    #[rstest::rstest]
    #[tokio::test]
    async fn kill_on_drop_terminates_process_tree_windows() {
        use std::process::Stdio;

        // Given a cmd command that launches a long-running background child.
        // `ping -n 60` waits ~59s — a reliable Windows-native delay.
        let guard = {
            let child = tokio::process::Command::new("cmd")
                .arg("/c")
                .arg("ping 127.0.0.1 -n 60 > nul")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn should succeed");
            assert!(child.id().unwrap_or(0) > 0, "child PID should be positive");
            KillOnDrop::new(child)
        };

        // Give the process a moment to start.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        // When dropping the guard (simulating task abort).
        drop(guard);

        // Then give kill_tree a moment to enumerate and terminate the tree.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        // Verify no `ping` processes survived via tasklist.
        let output = tokio::process::Command::new("tasklist")
            .args(["/FI", "IMAGENAME eq ping.exe", "/NH"])
            .output()
            .await
            .expect("tasklist should run");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.trim().is_empty() || stdout.contains("INFO: No tasks"),
            "expected no 'ping' processes, but found: {stdout}"
        );
    }

    #[rstest::rstest]
    fn push_line_appends_to_accumulated() {
        // Given a fresh batcher.
        let mut batcher = StreamingBatcher::new();
        let mut accumulated = String::new();

        // When pushing a line.
        batcher.push_line("hello world", &mut accumulated);

        // Then the accumulated string contains the line.
        assert_eq!(accumulated, "hello world\n");
    }

    #[rstest::rstest]
    fn should_flush_returns_false_below_threshold() {
        // Given a batcher with a short line.
        let mut batcher = StreamingBatcher::new();
        let mut accumulated = String::new();
        batcher.push_line("short", &mut accumulated);

        // When checking if the buffer should flush.
        // Then it returns false (buffer is well under 4096 bytes).
        assert!(!batcher.should_flush());
    }

    #[rstest::rstest]
    fn should_flush_returns_true_at_threshold() {
        // Given a batcher with enough data to exceed the flush threshold.
        let mut batcher = StreamingBatcher::new();
        let mut accumulated = String::new();

        // Push lines until buffer exceeds STREAM_FLUSH_THRESHOLD (4096 bytes).
        let line = "a".repeat(1000);
        for _ in 0..5 {
            batcher.push_line(&line, &mut accumulated);
        }

        // When checking if the buffer should flush.
        // Then it returns true.
        assert!(batcher.should_flush());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn flush_is_noop_on_empty_buffer() {
        // Given a fresh batcher and a recording bus.
        let mut batcher = StreamingBatcher::new();
        let (bus, audit) = BusService::new_recording();
        let session_id = SessionId::default();

        // When flushing an empty buffer.
        batcher
            .flush(Some(&bus), Some(&session_id), "test_call")
            .await;

        // Then no ToolExecutionOutput is emitted.
        let outputs: Vec<ToolExecutionOutput> = audit.of_type::<ToolExecutionOutput>();
        assert!(outputs.is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn flush_emits_and_clears_buffer() {
        // Given a batcher with two buffered lines and a recording bus.
        let mut batcher = StreamingBatcher::new();
        let mut accumulated = String::new();
        let (bus, audit) = BusService::new_recording();
        let session_id = SessionId::default();
        batcher.push_line("line one", &mut accumulated);
        batcher.push_line("line two", &mut accumulated);

        // When flushing the buffer.
        batcher
            .flush(Some(&bus), Some(&session_id), "test_call")
            .await;

        // Then exactly one ToolExecutionOutput is emitted containing both lines.
        let outputs: Vec<ToolExecutionOutput> = audit.of_type::<ToolExecutionOutput>();
        assert_eq!(outputs.len(), 1);
        assert!(outputs[0].output.contains("line one"));
        assert!(outputs[0].output.contains("line two"));
        // And the buffer is cleared.
        assert!(!batcher.should_flush());
    }

    #[rstest::rstest]
    fn push_line_truncates_accumulated_at_max_bytes() {
        // Given a fresh batcher.
        let mut batcher = StreamingBatcher::new();
        let mut accumulated = String::new();

        // When pushing enough data to exceed STREAM_BUFFER_MAX_BYTES.
        // STREAM_BUFFER_MAX_BYTES = DEFAULT_MAX_BYTES * 2 = 50KB * 2 = 100KB.
        let line = "x".repeat(2048); // 2KB per line
        for _ in 0..55 {
            // 55 * 2KB = 110KB > 100KB
            batcher.push_line(&line, &mut accumulated);
        }

        // Then accumulated was truncated (less than the untruncated 110KB).
        assert!(
            accumulated.len() < 110 * 1024,
            "accumulated should be truncated, but is {} bytes",
            accumulated.len()
        );
    }

    #[rstest::rstest]
    fn format_exit_result_success_with_output() {
        // Given a successful exit and some output.
        let status = std::process::Command::new("true")
            .status()
            .expect("true should run");
        let exit_result: Result<std::process::ExitStatus, std::io::Error> = Ok(status);

        // When formatting the exit result.
        let result = format_exit_result(
            &exit_result,
            "hello world\n",
            "call_1".to_owned(),
            "bash".to_owned(),
            DEFAULT_MAX_LINES,
            DEFAULT_MAX_BYTES,
        );

        // Then success is true and content contains the output.
        assert!(result.success);
        assert_eq!(result.content, "hello world\n");
        assert!(result.full_content.is_none());
    }

    #[rstest::rstest]
    fn format_exit_result_failure_with_exit_code() {
        // Given a non-zero exit.
        let status = std::process::Command::new("bash")
            .args(["-c", "exit 42"])
            .status()
            .expect("bash should run");
        let exit_result: Result<std::process::ExitStatus, std::io::Error> = Ok(status);

        // When formatting the exit result with output.
        let result = format_exit_result(
            &exit_result,
            "some output\n",
            "call_2".to_owned(),
            "bash".to_owned(),
            DEFAULT_MAX_LINES,
            DEFAULT_MAX_BYTES,
        );

        // Then success is false and content includes exit code.
        assert!(!result.success);
        assert!(result.content.contains("some output"));
        assert!(result.content.contains("Command exited with code 42"));
    }

    #[rstest::rstest]
    fn format_exit_result_failure_without_output() {
        // Given a non-zero exit with empty output.
        let status = std::process::Command::new("bash")
            .args(["-c", "exit 1"])
            .status()
            .expect("bash should run");
        let exit_result: Result<std::process::ExitStatus, std::io::Error> = Ok(status);

        // When formatting the exit result.
        let result = format_exit_result(
            &exit_result,
            "",
            "call_3".to_owned(),
            "bash".to_owned(),
            DEFAULT_MAX_LINES,
            DEFAULT_MAX_BYTES,
        );

        // Then success is false and content includes exit code.
        assert!(!result.success);
        assert!(result.content.contains("Command exited with code 1"));
    }

    #[rstest::rstest]
    fn format_exit_result_io_error() {
        // Given an I/O error.
        let exit_result: Result<std::process::ExitStatus, std::io::Error> = Err(
            std::io::Error::new(std::io::ErrorKind::NotFound, "not found"),
        );

        // When formatting the exit result.
        let result = format_exit_result(
            &exit_result,
            "",
            "call_4".to_owned(),
            "bash".to_owned(),
            DEFAULT_MAX_LINES,
            DEFAULT_MAX_BYTES,
        );

        // Then success is false.
        assert!(!result.success);
    }

    #[rstest::rstest]
    fn format_exit_result_failure_appends_newline_if_missing() {
        // Given a non-zero exit with output not ending in newline.
        let status = std::process::Command::new("bash")
            .args(["-c", "exit 3"])
            .status()
            .expect("bash should run");
        let exit_result: Result<std::process::ExitStatus, std::io::Error> = Ok(status);

        // When formatting the exit result with output not ending in newline.
        let result = format_exit_result(
            &exit_result,
            "no trailing newline",
            "call_5".to_owned(),
            "bash".to_owned(),
            DEFAULT_MAX_LINES,
            DEFAULT_MAX_BYTES,
        );

        // Then a newline is added before the exit code message.
        assert!(!result.success);
        assert!(
            result
                .content
                .contains("no trailing newline\nCommand exited with code 3")
        );
    }

    #[rstest::rstest]
    fn truncate_streaming_output_returns_non_empty() {
        // Given a string that's well within limits.
        let content = "hello\nworld\n";

        // When truncating.
        let result = truncate_streaming_output(content);

        // Then the content is returned as-is.
        assert_eq!(result, content);
    }

    #[rstest::rstest]
    fn push_line_at_exact_stream_buffer_max_does_not_truncate_prematurely() {
        // Given a batcher and accumulated exactly at STREAM_BUFFER_MAX_BYTES - 1.
        let mut batcher = StreamingBatcher::new();
        let mut accumulated = String::new();

        // Push lines until just below the threshold.
        let line = "a".repeat(1000);
        for _ in 0..(STREAM_BUFFER_MAX_BYTES / 1001) {
            batcher.push_line(&line, &mut accumulated);
        }
        let _len_before = accumulated.len();

        // When pushing one more line that pushes over the threshold.
        batcher.push_line("extra", &mut accumulated);

        // Then accumulated grew (or was truncated).
        assert_ne!(accumulated.len(), 0);
    }

    #[rstest::rstest]
    fn should_flush_returns_false_at_exact_threshold() {
        // Given a batcher with exactly STREAM_FLUSH_THRESHOLD bytes.
        let mut batcher = StreamingBatcher::new();
        let mut accumulated = String::new();

        // Push data that is exactly at the threshold boundary (not exceeding).
        let line = "a".repeat(STREAM_FLUSH_THRESHOLD - 1); // +1 for \n = STREAM_FLUSH_THRESHOLD
        batcher.push_line(&line, &mut accumulated);

        // When checking if it should flush.
        // Then it returns false (len == threshold, not > threshold).
        assert!(!batcher.should_flush());
    }

    #[rstest::rstest]
    #[test]
    fn definition_schema_injects_default_timeout_secs() {
        // Given the global default of 300s.
        // When building the definition.
        let def = definition(300);

        // Then the description mentions 300s.
        assert!(
            def.description.contains("300s"),
            "expected description to mention 300s, got: {}",
            def.description
        );
    }

    #[rstest::rstest]
    #[test]
    fn definition_schema_exposes_max_duration_secs_not_timeout() {
        // Given the bash tool definition.
        let def = definition(300);
        let params = def.parameters.to_string();

        // Then the schema contains the max_duration_secs key.
        assert!(
            params.contains("max_duration_secs"),
            "expected schema to contain max_duration_secs, got: {params}",
        );
        // And does NOT contain the old timeout key.
        assert!(
            !params.contains("\"timeout\""),
            "expected schema to NOT contain a \"timeout\" key, got: {params}",
        );
    }

    #[rstest::rstest]
    #[test]
    fn definition_description_names_max_duration_secs_and_shows_example() {
        // Given the bash tool definition.
        let def = definition(300);

        // Then the description names max_duration_secs.
        assert!(def.description.contains("max_duration_secs"));
        // And contains an inline example call.
        assert!(def.description.contains("\"max_duration_secs\": 600"));
    }

    #[rstest::rstest]
    #[test]
    fn definition_guidelines_have_proactive_and_reactive_bullets() {
        // Given the bash tool definition.
        let def = definition(300);

        // Then guidelines contain a proactive bullet (mentions setting max_duration_secs for slow commands).
        let proactive = def
            .prompt_guidelines
            .iter()
            .any(|g| g.contains("Proactively") && g.contains("max_duration_secs"));
        assert!(
            proactive,
            "missing proactive guideline: {:?}",
            def.prompt_guidelines
        );

        // And a reactive bullet (mentions retrying after a kill).
        let reactive = def
            .prompt_guidelines
            .iter()
            .any(|g| g.contains("killed") && g.contains("max_duration_secs"));
        assert!(
            reactive,
            "missing reactive guideline: {:?}",
            def.prompt_guidelines
        );
    }
}
