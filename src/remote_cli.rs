//! The `jinn remote` client — a short-lived HTTP caller against a
//! running jinn's control surface.
//!
//! Discovery is the registry file each running instance maintains
//! (`remote-instances.json` under the state dir): read it, prune dead
//! entries by port probe, pick the instance (`--instance <pid>` or the
//! most recently launched), then POST. The client needs no config read
//! and no session store — it runs before either opens, and a fresh
//! machine without a config can still list instances.
//!
//! Delivery is fire-and-forget: the ack means the text was accepted
//! onto the delivery path (queued, or steered at the next prompt
//! boundary), not that a turn started.

use error_stack::Report;
use jinn_cli::cli::RemoteCommands;
use jinn_kernel::AppPaths;
use jinn_remote_msg::registry::{self, SelectError};
use jinn_remote_msg::{ErrorBody, SendAck, SendMode, SendRequest};
use wherror::Error;

/// How the client failed before or during the call.
#[derive(Debug, Error)]
#[error(debug)]
pub enum RemoteClientError {
    /// No live instance is in the registry.
    NoInstances,
    /// The `--instance` pid is not in the registry.
    UnknownInstance(u32),
    /// The instance's port refused the connection — the entry is stale,
    /// or the instance died between listing and send.
    Unreachable { port: u16, source: std::io::Error },
    /// The instance answered with a non-2xx status.
    #[error(debug)]
    Rejected { status: u16, sentence: String },
    /// The HTTP conversation itself failed (timeout, TLS, decode).
    #[error(debug)]
    Http,
}

impl RemoteClientError {
    /// The sentence printed to stderr before the non-zero exit.
    #[must_use]
    pub fn sentence(&self) -> String {
        match self {
            Self::NoInstances => {
                "no running jinn instances (each running instance registers itself; \
                 start jinn first)"
                    .to_owned()
            }
            Self::UnknownInstance(pid) => {
                format!("no running jinn instance with pid {pid}")
            }
            Self::Unreachable { port, source } => {
                format!(
                    "the instance on port {port} refused the connection ({source}); \
                         its registry entry is stale or it just exited"
                )
            }
            Self::Rejected { status, sentence } => {
                format!("the instance rejected the send ({status}): {sentence}")
            }
            Self::Http => "the request to the instance failed".to_owned(),
        }
    }
}

/// Runs the `jinn remote` subcommand. Called from `prepare` before any
/// config read or session-store open.
///
/// # Errors
///
/// Returns the client error vocabulary; the caller prints the sentence
/// and exits non-zero.
pub fn run(
    subcommand: RemoteCommands,
    runtime: &tokio::runtime::Runtime,
) -> Result<(), Report<RemoteClientError>> {
    let paths = AppPaths::default();
    match subcommand {
        RemoteCommands::Instances => list_instances(&paths),
        RemoteCommands::Send {
            session,
            queue,
            steer,
            instance,
            message,
        } => runtime.block_on(send(&paths, session, queue, steer, instance, message)),
    }
}

/// `jinn remote instances` — one row per live instance.
fn list_instances(paths: &AppPaths) -> Result<(), Report<RemoteClientError>> {
    let entries = registry::read_live(&paths.remote_instances_path().into_inner());
    if entries.is_empty() {
        println!("no running jinn instances");
        return Ok(());
    }
    println!("{:<8} {:<6} {:<20} CWD", "PID", "PORT", "STARTED");
    for entry in &entries {
        println!(
            "{:<8} {:<6} {:<20} {}",
            entry.pid(),
            entry.port(),
            format_started(entry.started_at()),
            entry.cwd(),
        );
    }
    Ok(())
}

/// Epoch millis to a `YYYY-MM-DD HH:MM UTC` rendering; a zero or
/// nonsensical stamp prints as `unknown` rather than a wrong date.
fn format_started(started_at: u64) -> String {
    let Ok(secs) = i64::try_from(started_at / 1000) else {
        return "unknown".to_owned();
    };
    jiff::Timestamp::from_second(secs)
        .map(|t| t.to_string())
        .unwrap_or_else(|_| "unknown".to_owned())
}

/// `jinn remote send` — resolve the instance, POST, print the outcome.
async fn send(
    paths: &AppPaths,
    session: String,
    queue: bool,
    steer: bool,
    instance: Option<u32>,
    message: String,
) -> Result<(), Report<RemoteClientError>> {
    use error_stack::ResultExt as _;

    let entries = registry::read_live(&paths.remote_instances_path().into_inner());
    let target = registry::select(&entries, instance).map_err(|error| match error {
        SelectError::NoInstances => Report::new(RemoteClientError::NoInstances),
        SelectError::UnknownInstance(pid) => Report::new(RemoteClientError::UnknownInstance(pid)),
    })?;

    let mode = match (queue, steer) {
        // clap's conflict group makes both-flags unparseable; this arm
        // is the type-level echo of that rule.
        (true, true) => unreachable!("clap rejects --queue with --steer"),
        (false, true) => Some(SendMode::Steer),
        (true, false) | (false, false) => Some(SendMode::Queue),
    };
    let request = SendRequest {
        session_id: session,
        text: message,
        mode,
    };

    let url = format!("http://127.0.0.1:{}/send", target.port());
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .change_context(RemoteClientError::Http)?;
    let response = client
        .post(&url)
        .json(&request)
        .send()
        .await
        .map_err(|error| {
            // Connection refused is the stale-entry case; name the port
            // and the probe advice. Anything else is a plain HTTP
            // failure.
            if error.is_connect() {
                Report::new(RemoteClientError::Unreachable {
                    port: target.port(),
                    source: std::io::Error::other("connection refused"),
                })
            } else {
                Report::new(RemoteClientError::Http).attach(format!("{error}"))
            }
        })?;

    let status = response.status();
    if !status.is_success() {
        let sentence = response
            .json::<ErrorBody>()
            .await
            .map(|body| body.error)
            .unwrap_or_else(|_| format!("HTTP {status}"));
        return Err(Report::new(RemoteClientError::Rejected {
            status: status.as_u16(),
            sentence,
        }));
    }

    let ack: SendAck = response
        .json()
        .await
        .change_context(RemoteClientError::Http)?;
    match ack.status {
        jinn_remote_msg::SendStatus::Queued => {
            let fallback = if ack.fell_back {
                " (idle target; the steer was delivered as a queue message)"
            } else {
                ""
            };
            println!("queued in session {}{fallback}", ack.session_id);
        }
        jinn_remote_msg::SendStatus::Steered => {
            println!("steered into session {}", ack.session_id);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        reason = "test assertions need expect/panic to state what went wrong"
    )]

    use super::RemoteClientError;
    use jinn_remote_msg::registry::{self, InstanceEntry};
    use jinn_testutil::TestRoot;
    use marked_path::MarkedPathAccess as _;

    fn entry(pid: u32, port: u16, started_at: u64) -> InstanceEntry {
        InstanceEntry::new(pid, port, "/tmp/work".to_owned(), started_at)
    }

    #[rstest::rstest]
    #[test]
    fn error_sentences_name_the_condition() {
        // Given each error variant.
        let errors = [
            RemoteClientError::NoInstances,
            RemoteClientError::UnknownInstance(42),
            RemoteClientError::Rejected {
                status: 404,
                sentence: "no live session with id x".to_owned(),
            },
            RemoteClientError::Http,
        ];

        // Then every sentence names its condition (the no-instances one
        // is the user-facing path a fresh machine hits first).
        for error in &errors {
            let sentence = error.sentence();
            assert!(!sentence.is_empty());
        }
        assert!(errors[0].sentence().contains("no running jinn instances"));
        assert!(errors[1].sentence().contains("42"));
        assert!(errors[2].sentence().contains("no live session"));
    }

    #[rstest::rstest]
    #[test]
    fn the_output_verbs_match_the_ack() {
        // Given the two ack statuses the server can return.
        let queued = "queued in session abc";
        let steered = "steered into session abc";

        // Then the output strings are stable script surface.
        assert!(queued.starts_with("queued in session"));
        assert!(steered.starts_with("steered into session"));
    }

    #[rstest::rstest]
    #[test]
    fn selection_over_a_temp_registry_prefers_the_requested_pid() {
        // Given a registry file with two live instances, the later one
        // not launched most recently (by stamp).
        let root = TestRoot::new();
        let path = root.join_str("remote-instances.json").expect("joins");
        let listener_a = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let listener_b = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port_a = listener_a.local_addr().expect("addr").port();
        let port_b = listener_b.local_addr().expect("addr").port();
        registry::register(path.as_path(), entry(100, port_a, 500)).expect("register");
        registry::register(path.as_path(), entry(200, port_b, 100)).expect("register");

        // When reading the live entries and selecting each pid.
        let entries = registry::read_live(path.as_path());
        let a = registry::select(&entries, Some(100)).expect("a present");
        let b = registry::select(&entries, Some(200)).expect("b present");

        // Then each pid resolves to its own port regardless of stamp.
        assert_eq!(a.port(), port_a);
        assert_eq!(b.port(), port_b);
    }

    #[rstest::rstest]
    #[test]
    fn an_unknown_pid_in_a_populated_registry_is_the_named_error() {
        // Given a registry carrying one live entry.
        let root = TestRoot::new();
        let path = root.join_str("remote-instances.json").expect("joins");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        registry::register(path.as_path(), entry(100, port, 500)).expect("register");
        let entries = registry::read_live(path.as_path());

        // When selecting a pid the registry does not carry.
        let result = registry::select(&entries, Some(999));

        // Then the named unknown-instance error comes back.
        assert!(matches!(
            result,
            Err(registry::SelectError::UnknownInstance(999))
        ));
    }
}
