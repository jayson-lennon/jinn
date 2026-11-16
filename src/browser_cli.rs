//! The `jinn browser` client commands — start, stop, status, restart.
//!
//! Registry + loopback HTTP only: these commands never read the config
//! document and never open the session store (the interception in
//! `bootstrap::config` guarantees the ordering). `start` runs the same
//! auto-start path every client rides, so an explicit `jinn browser
//! start` and an MCP tool's lazy start are one code path.

use error_stack::Report;
use jinn_browser::client::DaemonClient;
use jinn_cli::cli::BrowserCommands;

/// Runs one client subcommand, returning the user-facing output line.
///
/// # Errors
///
/// Client failures (spawn failure, unreachable daemon) are Reports; the
/// caller renders them like every other app error.
pub fn run(command: &BrowserCommands) -> Result<String, Report<BrowserClientError>> {
    // The blocking client is deliberately sync: each call is short and
    // the client commands are one-shot.
    let client = DaemonClient::new();
    let outcome: Result<String, Report<BrowserClientError>> = match command {
        BrowserCommands::Start => {
            let entry = client.ensure_daemon().map_err(|e| {
                e.change_context(BrowserClientError::Wrapped(
                    jinn_browser::client::ClientError::Unavailable(String::new()),
                ))
            })?;
            Ok(format!(
                "browser daemon running (pid {}, port {})",
                entry.pid(),
                entry.port()
            ))
        }
        BrowserCommands::Stop => {
            client.shutdown().map_err(|e| {
                e.change_context(BrowserClientError::Wrapped(
                    jinn_browser::client::ClientError::Unavailable(String::new()),
                ))
            })?;
            Ok("browser daemon stopped".to_owned())
        }
        BrowserCommands::Status => match client.health() {
            Ok(health) => Ok(format!(
                "browser daemon: {} (binary {}, {}, up {}s)",
                health.browser,
                health.binary,
                health.binary_source,
                health.uptime_ms / 1000
            )),
            Err(_) => Ok("browser daemon: not running".to_owned()),
        },
        BrowserCommands::Restart => {
            // Stop is best effort: a not-running daemon is the same
            // postcondition as a stopped one.
            let _ = client.shutdown();
            let entry = client.ensure_daemon().map_err(|e| {
                e.change_context(BrowserClientError::Wrapped(
                    jinn_browser::client::ClientError::Unavailable(String::new()),
                ))
            })?;
            Ok(format!(
                "browser daemon restarted (pid {}, port {})",
                entry.pid(),
                entry.port()
            ))
        }
        BrowserCommands::Daemon { .. } => {
            // Unreachable: the interception routes Daemon to
            // `Prepared::ServeBrowserDaemon`, never here. Handled anyway
            // so the match is total.
            Err(Report::new(BrowserClientError::Wrapped(
                jinn_browser::client::ClientError::Unavailable(
                    "`browser daemon` is a serve command, not a client command; \
                     this path is a wiring bug"
                        .to_owned(),
                ),
            )))
        }
    };
    outcome
}

/// How a client command failed.
#[derive(Debug, wherror::Error)]
#[error(debug)]
pub enum BrowserClientError {
    /// A daemon client failure (spawn, lock, unreachable, tool error),
    /// wrapped so the app-level context chain stays one type.
    #[error(transparent)]
    Wrapped(#[from] jinn_browser::client::ClientError),
}
