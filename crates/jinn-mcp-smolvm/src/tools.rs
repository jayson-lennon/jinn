//! `vm_run` and `vm_info`: the request-to-exec translation the MCP tools
//! share.
//!
//! Pure logic, no rmcp: the tool descriptions, argument validation,
//! `bash -lc` wrapping, and the `vm_info` probe script live here so tests
//! drive the translation directly. The rmcp handler in `server.rs` parses
//! the JSON arguments, calls into this module, and renders the outcome.

use std::time::Duration;

use error_stack::Report;
use wherror::Error;

use crate::backend::{ExecOptions, ExecOutcome, SmolvmBackend};

/// The default timeout for `vm_run`, when `timeout_secs` is absent.
pub(crate) const DEFAULT_TIMEOUT_SECS: u64 = 60;

/// The hard timeout cap for `vm_run`.
///
/// Inside jinn's MCP tool timeout (300s default) sits this tool-level cap;
/// the guest kill must happen inside the tool so partial output survives —
/// the MCP layer timing out instead would drop it.
pub(crate) const MAX_TIMEOUT_SECS: u64 = 600;

/// The workdir `vm_run` uses when none is given: the fixed guest mount.
pub(crate) const DEFAULT_WORKDIR: &str = "/workspace";

/// Why a `vm_run` request cannot run.
#[derive(Debug, Clone, Error)]
#[error(debug)]
pub(crate) enum RunError {
    /// `timeout_secs` was outside the 1..=600 range.
    TimeoutOutOfRange {
        /// The rejected value.
        requested: u64,
    },
    /// `workdir` was a relative guest path.
    RelativeWorkdir {
        /// The rejected value.
        workdir: String,
    },
}

impl RunError {
    /// Renders the failure as tool-level error text the model can read and
    /// correct.
    pub(crate) fn message(self) -> String {
        match self {
            Self::TimeoutOutOfRange { requested } => format!(
                "vm_run failed: timeout_secs must be between 1 and {MAX_TIMEOUT_SECS}, got {requested}"
            ),
            Self::RelativeWorkdir { workdir } => format!(
                "vm_run failed: workdir must be an absolute guest path (inside the VM), got `{workdir}`"
            ),
        }
    }
}

/// Renders a failed `vm_run` call as tool-level error text: the typed
/// validation failure when that is what failed, else the report's
/// attachments (the engine's message rides them).
pub(crate) fn run_failure_message(failure: &RunFailure) -> String {
    match failure {
        RunFailure::Validation(report) => RunError::message(report.current_context().clone()),
        RunFailure::Guest(report) => format!("vm_run failed: {report:?}"),
    }
}

/// Builds the exec options for a `vm_run` request.
///
/// # Errors
///
/// Returns [`Report<RunError>`] when `timeout_secs` is outside
/// `1..=600`, or `workdir` is a relative guest path.
pub(crate) fn run_options(
    timeout_secs: Option<u64>,
    workdir: Option<&str>,
) -> Result<ExecOptions, Report<RunError>> {
    let timeout_secs = match timeout_secs {
        None => DEFAULT_TIMEOUT_SECS,
        Some(secs) if (1..=MAX_TIMEOUT_SECS).contains(&secs) => secs,
        Some(requested) => return Err(Report::new(RunError::TimeoutOutOfRange { requested })),
    };

    if workdir.is_some_and(|workdir| !workdir.starts_with('/')) {
        return Err(Report::new(RunError::RelativeWorkdir {
            workdir: workdir.unwrap_or_default().to_owned(),
        }));
    }

    Ok(ExecOptions {
        workdir: Some(workdir.map_or_else(|| DEFAULT_WORKDIR.to_owned(), str::to_owned)),
        timeout: Some(Duration::from_secs(timeout_secs)),
    })
}

/// Wraps a shell string as the argv the guest executes: `bash -lc <cmd>`.
/// The login shell (`-l`) sources `/etc/profile`, so PATH carries the
/// init-installed toolchain.
#[must_use]
pub(crate) fn wrap_shell_command(cmd: &str) -> Vec<String> {
    vec!["bash".to_owned(), "-lc".to_owned(), cmd.to_owned()]
}

/// Runs `cmd` through the backend with `vm_run`'s contract.
///
/// # Errors
///
/// Returns [`Report<RunError>`] on invalid arguments; a backend failure
/// propagates as the backend error's own report (the engine's message
/// rides its attachments).
pub(crate) fn run_command(
    backend: &dyn SmolvmBackend,
    cmd: &str,
    timeout_secs: Option<u64>,
    workdir: Option<&str>,
) -> Result<ExecOutcome, RunFailure> {
    let options = run_options(timeout_secs, workdir).map_err(RunFailure::Validation)?;
    let argv = wrap_shell_command(cmd);
    backend
        .exec(argv, options)
        .map_err(|report| RunFailure::Guest(report.change_context(GuestError)))
}

/// Why a `vm_run` call failed, distinguishing a rejected request from a
/// failing guest: the first is the model's error to fix, the second is
/// reported with the engine's own message.
#[derive(Debug, Error)]
#[error(debug)]
pub(crate) enum RunFailure {
    /// The request was invalid (timeout range, relative workdir); the
    /// report carries the typed [`RunError`] as its context.
    Validation(error_stack::Report<RunError>),
    /// The machine could not run the command at all; the report carries
    /// [`GuestError`] plus the engine's message.
    Guest(error_stack::Report<GuestError>),
}

/// Marks a guest-side failure in a `vm_run` call.
#[derive(Debug, Error)]
#[error(debug)]
pub(crate) struct GuestError;

/// The one exec `vm_info` runs to probe the live guest: distro, kernel,
/// cpus, memory, the workspace mount line, and uptime.
pub(crate) const GUEST_FACTS_SCRIPT: &str = r#"
echo "OS: $(. /etc/os-release 2>/dev/null && echo "$PRETTY_NAME")"
echo "Kernel: $(uname -r)"
echo "CPUs: $(nproc)"
echo "Memory: $(awk '/MemTotal/ {printf "%d MiB", $2/1024}' /proc/meminfo)"
echo "Workspace: $(grep ' /workspace ' /proc/mounts | head -1 | cut -d' ' -f3 || echo 'not mounted')"
echo "Uptime: $(uptime -p 2>/dev/null || uptime)"
"#;

/// Builds the `vm_info` text block: live guest facts first, then the
/// image and the manifest path with its verbatim contents.
#[must_use]
pub(crate) fn info_text(
    facts: Option<&str>,
    backend: &dyn SmolvmBackend,
    manifest_path: &str,
    manifest_contents: &str,
) -> String {
    let mut text = String::from("smolvm guest\n============\n");
    match facts {
        Some(facts) => text.push_str(facts.trim_end()),
        None => text.push_str("(live facts unavailable: the guest probe failed)"),
    }
    text.push_str("\nimage: ");
    text.push_str(&backend.guest_image());
    text.push_str("\n\nmanifest: ");
    text.push_str(manifest_path);
    text.push('\n');
    text.push_str(manifest_contents.trim_end());
    text.push('\n');
    text
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unwrap_used,
        reason = "test code"
    )]
    #![allow(
        clippy::indexing_slicing,
        reason = "test code asserts the recorded call exists before indexing it"
    )]

    use super::*;
    use std::sync::Mutex;

    /// What the fake backend recorded from one call.
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct RecordedCall {
        argv: Vec<String>,
        workdir: Option<String>,
        timeout: Option<Duration>,
    }

    /// A fake backend: records the call, returns a canned outcome or a
    /// canned failure.
    struct FakeBackend {
        calls: Mutex<Vec<RecordedCall>>,
        outcome: ExecOutcome,
        fail: bool,
    }

    impl FakeBackend {
        fn with_outcome(outcome: ExecOutcome) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                outcome,
                fail: false,
            }
        }

        fn failing() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                outcome: ExecOutcome {
                    stdout: String::new(),
                    stderr: String::new(),
                    exit_code: 0,
                },
                fail: true,
            }
        }

        fn calls(&self) -> Vec<RecordedCall> {
            self.calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        }
    }

    impl SmolvmBackend for FakeBackend {
        fn exec(
            &self,
            argv: Vec<String>,
            options: ExecOptions,
        ) -> Result<ExecOutcome, Report<crate::backend::BackendError>> {
            use error_stack::Report;
            self.calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(RecordedCall {
                    argv,
                    workdir: options.workdir,
                    timeout: options.timeout,
                });
            if self.fail {
                return Err(Report::new(crate::backend::BackendError)
                    .attach("fake guest failure: engine went away"));
            }
            Ok(self.outcome.clone())
        }

        fn guest_image(&self) -> String {
            "alpine:latest".to_owned()
        }
    }

    fn echo_outcome() -> ExecOutcome {
        ExecOutcome {
            stdout: "hello\n".to_owned(),
            stderr: String::new(),
            exit_code: 0,
        }
    }

    #[rstest::rstest]
    #[test]
    fn run_command_wraps_the_shell_string_as_bash_lc() {
        // Given a fake backend.
        let backend = FakeBackend::with_outcome(echo_outcome());

        // When running a shell command.
        run_command(&backend, "echo hello", None, None).expect("run");

        // Then the guest received the bash -lc wrapping, not the raw string.
        let calls = backend.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].argv,
            vec!["bash".to_owned(), "-lc".to_owned(), "echo hello".to_owned()]
        );
    }

    #[rstest::rstest]
    #[test]
    fn run_command_defaults_to_the_workspace_workdir() {
        // Given a fake backend and no workdir argument.
        let backend = FakeBackend::with_outcome(echo_outcome());

        // When running a command.
        run_command(&backend, "pwd", None, None).expect("run");

        // Then the exec options carried /workspace.
        let calls = backend.calls();
        assert_eq!(calls[0].workdir.as_deref(), Some("/workspace"));
    }

    #[rstest::rstest]
    #[test]
    fn run_command_defaults_to_a_60_second_timeout() {
        // Given a fake backend and no timeout argument.
        let backend = FakeBackend::with_outcome(echo_outcome());

        // When running a command.
        run_command(&backend, "sleep 5", None, None).expect("run");

        // Then the exec options carried 60 seconds.
        let calls = backend.calls();
        assert_eq!(calls[0].timeout, Some(Duration::from_secs(60)));
    }

    #[rstest::rstest]
    #[test]
    fn run_command_honors_an_explicit_workdir_and_timeout() {
        // Given a fake backend.
        let backend = FakeBackend::with_outcome(echo_outcome());

        // When running with explicit workdir and timeout.
        run_command(&backend, "ls", Some(120), Some("/tmp")).expect("run");

        // Then both reached the exec options unchanged.
        let calls = backend.calls();
        assert_eq!(calls[0].workdir.as_deref(), Some("/tmp"));
        assert_eq!(calls[0].timeout, Some(Duration::from_secs(120)));
    }

    #[rstest::rstest]
    #[test]
    fn timeout_secs_outside_the_range_is_rejected_naming_the_bound() {
        // Given the 1..=600 range.
        let backend = FakeBackend::with_outcome(echo_outcome());

        // When requesting a timeout above the cap.
        let failure =
            run_command(&backend, "ls", Some(601), None).expect_err("over-cap timeout must fail");

        // Then the failure is a validation failure naming the bound, and
        // nothing reached the guest.
        let message = run_failure_message(&failure);
        assert!(
            message.contains("between 1 and 600"),
            "names the range: {message}"
        );
        assert!(message.contains("601"), "names the value: {message}");
        assert_eq!(
            backend.calls(),
            [] as [RecordedCall; 0],
            "no guest call was made"
        );
    }

    #[rstest::rstest]
    #[test]
    fn zero_timeout_secs_is_rejected() {
        // Given the 1..=600 range.
        let backend = FakeBackend::with_outcome(echo_outcome());

        // When requesting a zero timeout.
        let failure =
            run_command(&backend, "ls", Some(0), None).expect_err("zero timeout must fail");

        // Then the failure is a validation failure naming the value.
        let message = run_failure_message(&failure);
        assert!(message.contains("got 0"), "names the value: {message}");
        assert_eq!(backend.calls(), [] as [RecordedCall; 0]);
    }

    #[rstest::rstest]
    #[test]
    fn a_relative_workdir_is_rejected_naming_the_rule() {
        // Given the absolute-guest-path rule.
        let backend = FakeBackend::with_outcome(echo_outcome());

        // When running with a relative workdir.
        let failure = run_command(&backend, "ls", None, Some("relative/dir"))
            .expect_err("relative workdir must fail");

        // Then the failure names the rule and the value, and nothing
        // reached the guest.
        let message = run_failure_message(&failure);
        assert!(
            message.contains("absolute guest path"),
            "names the rule: {message}"
        );
        assert!(
            message.contains("relative/dir"),
            "names the value: {message}"
        );
        assert_eq!(backend.calls(), [] as [RecordedCall; 0]);
    }

    #[rstest::rstest]
    #[test]
    fn a_guest_failure_rides_the_engine_message_as_tool_text() {
        // Given a backend that fails at the engine layer.
        let backend = FakeBackend::failing();

        // When running a command.
        let failure = run_command(&backend, "ls", None, None).expect_err("guest failure must fail");

        // Then the tool-level message carries the engine's own words.
        let message = run_failure_message(&failure);
        assert!(
            message.contains("fake guest failure"),
            "carries the engine message: {message}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn info_text_leads_with_facts_and_carries_the_manifest_verbatim() {
        // Given a fake backend, a facts string, and a manifest.
        let backend = FakeBackend::with_outcome(echo_outcome());
        let facts = "OS: Alpine Linux\nKernel: 6.6.0-0";
        let manifest_contents = "image = \"alpine:latest\"\ncpus = 2\n";

        // When building the info text.
        let text = info_text(
            Some(facts),
            &backend,
            "/root/.config/jinn/smolvm/alpine.toml",
            manifest_contents,
        );

        // Then the live facts come first.
        assert!(text.starts_with("smolvm guest"));
        assert!(text.contains("OS: Alpine Linux"));
        // And the manifest path and verbatim contents follow.
        assert!(text.contains("manifest: /root/.config/jinn/smolvm/alpine.toml"));
        assert!(text.contains("image = \"alpine:latest\""));
        // And the image the machine booted is named.
        assert!(text.contains("image: alpine:latest"));
    }

    #[rstest::rstest]
    #[test]
    fn info_text_says_so_when_the_facts_probe_failed() {
        // Given a probe that produced no facts.
        let backend = FakeBackend::with_outcome(echo_outcome());

        // When building the info text without facts.
        let text = info_text(
            None,
            &backend,
            "/root/.config/jinn/smolvm/alpine.toml",
            "image = \"alpine:latest\"\ncpus = 2\n",
        );

        // Then the block says the probe failed instead of showing nothing.
        assert!(
            text.contains("live facts unavailable"),
            "names the probe failure: {text}"
        );
        // And the manifest block still serves.
        assert!(text.contains("manifest: /root/.config/jinn/smolvm/alpine.toml"));
    }

    #[rstest::rstest]
    #[test]
    fn the_guest_facts_script_probes_distro_kernel_cpus_memory_mount_uptime() {
        // Given the shipped probe script.
        // When scanning its echoed labels.
        // Then it covers every fact vm_info documents.
        assert!(GUEST_FACTS_SCRIPT.contains("\"OS:"));
        assert!(GUEST_FACTS_SCRIPT.contains("\"Kernel:"));
        assert!(GUEST_FACTS_SCRIPT.contains("\"CPUs:"));
        assert!(GUEST_FACTS_SCRIPT.contains("\"Memory:"));
        assert!(GUEST_FACTS_SCRIPT.contains("\"Workspace:"));
        assert!(GUEST_FACTS_SCRIPT.contains("\"Uptime:"));
    }
}
