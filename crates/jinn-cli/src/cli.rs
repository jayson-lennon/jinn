//! Command-line interface argument definitions.
//
// Every path flag in this module is the argv boundary: clap hands over raw,
// unvalidated values that `resolve_cli_path`/`ConfigPath` re-type into
// `MarkedPathBuf<Absolute>` during startup. No jinn logic consumes them raw.
#![allow(
    clippy::disallowed_types,
    reason = "this module is the argv/OsString boundary; every path flag is re-typed by resolve_cli_path at startup"
)]

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use clap_verbosity_flag::{Verbosity, WarnLevel};
use wherror::Error;

/// jinn - a TUI agent harness with a component/actor system.
#[derive(Debug, Parser)]
#[command(name = "jinn", version, about)]
pub struct Cli {
    /// Verbosity level for logging.
    #[command(flatten)]
    pub verbosity: Verbosity<WarnLevel>,

    /// Colorize trace output written to the log file.
    ///
    /// Traces only ever go to the log file (the terminal never shows traces in
    /// TUI mode), so this only affects file rendering. Default is plain text.
    #[arg(long, global = true)]
    pub trace_color: bool,

    /// Path to the log file. Defaults to the platform's state directory
    /// (e.g. `~/.local/state/jinn/jinn.log` on Linux).
    ///
    /// A relative path resolves against the process cwd; a leading `~`
    /// expands against the home directory.
    #[arg(long, value_hint = clap::ValueHint::FilePath)]
    pub log_file: Option<PathBuf>,

    /// Trim the log file to its last N complete lines at startup.
    ///
    /// `0` keeps everything: the file is never trimmed and grows unbounded
    /// across restarts. When the flag is omitted, the historical default
    /// (10,000 lines) applies.
    #[arg(long, value_name = "N")]
    pub log_tail_lines: Option<usize>,

    /// Path to the `jinn.toml` configuration file. Defaults to the platform's
    /// config directory (e.g. `~/.config/jinn/jinn.toml` on Linux).
    ///
    /// The override applies to both reads and writes for the whole run, so the
    /// run's config stays a single coherent source of truth. A run that reads
    /// the config requires the file to already exist; `jinn config init` is
    /// the exception, since creating the file is what it is for.
    #[arg(long, global = true, value_hint = clap::ValueHint::FilePath)]
    pub config: Option<PathBuf>,

    /// Session database file. Defaults to the platform data directory.
    /// A relative path resolves against the process cwd; a leading `~`
    /// expands against the home directory.
    ///
    /// In debug builds this flag is **required** to prevent accidental use
    /// of the production database during development — except when the
    /// command is a bundled MCP server ([`Commands::Mcp`]): those children
    /// never open the session store, and the shipped config spawns them
    /// without a `--db-path`.
    #[arg(long, value_hint = clap::ValueHint::FilePath)]
    pub db_path: Option<PathBuf>,

    /// Dump every provider generation request to <dir> as a separate JSON file.
    /// Each file contains the complete request payload verbatim. A relative
    /// path resolves against the process cwd; a leading `~` expands against
    /// the home directory.
    #[arg(long, value_hint = clap::ValueHint::DirPath)]
    pub dump_requests: Option<PathBuf>,

    /// The subcommand to run. If omitted, launches the TUI.
    #[command(subcommand)]
    pub command: Option<Commands>,
}

impl Cli {
    /// Returns the database path if provided.
    ///
    /// In debug builds a missing `--db-path` on a non-MCP command is an
    /// error; see [`Self::debug_db_path_required`], which enforces the rule
    /// post-parse so the bundled MCP subcommands stay spawnable without the
    /// flag.
    pub fn db_path_opt(&self) -> Option<&PathBuf> {
        self.db_path.as_ref()
    }

    /// Debug-build guard: `--db-path` is required unless the command is a
    /// bundled MCP server (`jinn mcp …`, which never opens the session
    /// store), the external-remote client (`jinn remote …`, an HTTP
    /// client that needs no session store), or a dictation utility
    /// (`jinn dictation …`, which reads the audio host, not the store).
    /// Release builds always pass.
    ///
    /// # Errors
    ///
    /// Returns an error naming the missing flag when the rule is violated.
    pub fn debug_db_path_required(&self) -> Result<(), DebugDbPathError> {
        if cfg!(debug_assertions)
            && self.db_path.is_none()
            && !matches!(
                self.command,
                Some(Commands::Mcp { .. } | Commands::Remote { .. } | Commands::Dictation { .. })
            )
        {
            return Err(DebugDbPathError);
        }
        Ok(())
    }
}

/// A debug build ran a session-store command without the required `--db-path`.
#[derive(Debug, Error)]
#[error(
    "--db-path is required in debug builds (development must not touch the \
     production database); bundled MCP servers (`jinn mcp …`) are exempt"
)]
pub struct DebugDbPathError;

/// Available subcommands.
#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Launch the TUI (default when no subcommand is given).
    Tui,

    /// Run without a terminal interface.
    #[cfg(debug_assertions)]
    Headless {
        /// Headless subcommand.
        #[command(subcommand)]
        command: Option<HeadlessCommands>,
    },

    /// Generate shell completions.
    Completions {
        /// The shell to generate completions for.
        shell: clap_complete::Shell,
    },

    /// Seed the default config files (jinn.toml, providers.toml,
    /// stream_rules.toml), each only when missing.
    ///
    /// Bundled resources (themes, personas, prompts, workflows, skills) are
    /// reconciled into place on every launch; use `--builtins` to
    /// force-restore them over local edits.
    Install {
        /// Overwrite every bundled resource with the shipped copy,
        /// reclaiming files you have edited. Config files are untouched.
        #[arg(long)]
        builtins: bool,
    },

    /// Fetch reference data from external sources.
    Fetch {
        /// The fetch subcommand to run.
        #[command(subcommand)]
        subcommand: FetchCommands,
    },

    /// Dictation utilities.
    Dictation {
        /// The dictation subcommand to run.
        #[command(subcommand)]
        subcommand: DictationCommands,
    },

    /// Manage user configuration files.
    Config {
        /// The config subcommand to run.
        #[command(subcommand)]
        subcommand: ConfigCommands,
    },

    /// Serve a bundled MCP server.
    ///
    /// Each subcommand runs one bundled server as a child process over
    /// stdio, driven by jinn's MCP subsystem (see `[mcp]` entries in
    /// jinn.toml). Not a TUI command: the process stays on the terminal
    /// until the client disconnects.
    Mcp {
        /// The mcp subcommand to run.
        #[command(subcommand)]
        subcommand: McpCommands,
    },

    /// Talk to a running jinn instance from outside.
    ///
    /// Discovers instances through the registry file each running jinn
    /// maintains (pid, port, working directory) and delivers a message
    /// into a live session. Not a TUI command: a short-lived HTTP
    /// client that needs no config or session store.
    Remote {
        /// The remote subcommand to run.
        #[command(subcommand)]
        subcommand: RemoteCommands,
    },
}

/// External-remote client subcommands.
#[derive(Debug, Clone, Subcommand)]
pub enum RemoteCommands {
    /// Deliver a message into a live session of a running jinn.
    Send {
        /// The target session's id (`jinn remote sessions` lists them).
        #[arg(long)]
        session: String,

        /// Queue the message behind any in-flight turn (the default).
        #[arg(long, group = "remote_mode")]
        queue: bool,

        /// Steer the running turn at its next prompt-assembly boundary.
        /// A steer aimed at an idle session is delivered as a queue
        /// message instead.
        #[arg(long, group = "remote_mode")]
        steer: bool,

        /// The target instance's pid (`jinn remote instances` lists
        /// them). Omit to target the most recently launched instance.
        #[arg(long)]
        instance: Option<u32>,

        /// The message text.
        message: String,
    },

    /// List the running jinn instances a client can reach.
    Instances,
}

/// MCP server subcommands.
///
/// One variant per bundled server; variants are added here alongside
/// the server they spawn.
#[derive(Debug, Subcommand)]
pub enum McpCommands {
    /// Does nothing; starts jinn normally.
    Noop,
    /// Serve the builtin overgraph graph database over stdio MCP.
    Overgraph {
        /// Path to the graph database directory (default: jinn-managed
        /// location under the app data dir).
        #[arg(long)]
        db: Option<std::path::PathBuf>,
        /// Embedding model for the vector tools (default:
        /// bge-small-en-v1.5).
        ///
        /// The model's dimension is locked into the database permanently at
        /// the first open, so switching models later fails at startup unless
        /// pointed at a fresh `--db`.
        #[arg(long)]
        embed_model: Option<String>,
        /// Download the embedding model to the local cache and exit,
        /// without serving.
        #[arg(long)]
        fetch_model: bool,
    },
}

/// Headless subcommands.
#[derive(Debug, Subcommand)]
pub enum HeadlessCommands {
    /// Send a chat message.
    SendChat {
        /// The message text to send.
        message: String,
    },
    /// Run a keystroke script.
    Script {
        /// Path to a script file with one key sequence per line.
        path: String,
    },
}

/// Fetch subcommands.
#[derive(Debug, Subcommand)]
pub enum FetchCommands {
    /// Fetch model metadata from models.dev and save locally.
    Models,

    /// Download the dictation model into jinn's own cache directory.
    ///
    /// Fetches regardless of `[dictation] enabled` — fetching is a
    /// network operation, enabling is a config decision. Exits
    /// non-zero on platforms without a real backend.
    DictationModel,
}

/// Dictation subcommands.
#[derive(Debug, Subcommand)]
pub enum DictationCommands {
    /// List the capture input devices, marking the system default.
    ///
    /// The printed names are exactly what a `[dictation] device`
    /// value must equal — matching is strict, not fuzzy. Runs
    /// regardless of `[dictation] enabled`. Exits non-zero on
    /// platforms without a real backend.
    ListDevices,
}

/// Config subcommands.
#[derive(Debug, Subcommand)]
pub enum ConfigCommands {
    /// Write the default jinn.toml to disk.
    ///
    /// Refuses to overwrite an existing file unless --force is given.
    Init {
        /// Overwrite the file if it already exists.
        #[arg(long)]
        force: bool,
    },
    /// Write the default commented providers.toml template to disk.
    ///
    /// Refuses to overwrite an existing file unless --force is given.
    Providers {
        /// Overwrite the file if it already exists.
        #[arg(long)]
        force: bool,
    },
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        reason = "test code: assertions use expect/panic to state what went wrong"
    )]
    use super::*;
    use clap::Parser;
    use marked_path::MarkedPathAccess;

    #[rstest::rstest]
    #[test]
    fn log_file_flag_defaults_to_none() {
        // Given a temp-rooted --db-path and no --log-file argument.
        let dir = jinn_testutil::tempdir();
        let db_path = dir
            .path
            .join("test.db")
            .expect("temp root joins a bare name")
            .to_string();

        // When parsing.
        let cli = Cli::parse_from(["jinn", "--db-path", db_path.as_str()]);

        // Then Cli.log_file is None (default).
        assert!(cli.log_file.is_none());
    }

    #[rstest::rstest]
    #[test]
    fn log_file_flag_global_overrides() {
        // Given a global --log-file argument before a subcommand.
        let dir = jinn_testutil::tempdir();
        let expected = dir.path.join("x.log").expect("temp root joins a bare name");
        let log_file = expected.to_string_lossy().to_string();
        let db_path = dir
            .path
            .join("test.db")
            .expect("temp root joins a bare name")
            .to_string();

        // When parsing.
        let cli = Cli::parse_from([
            "jinn",
            "--db-path",
            db_path.as_str(),
            "--log-file",
            log_file.as_str(),
            "tui",
        ]);

        // Then Cli.log_file captures the override.
        assert_eq!(cli.log_file.as_deref(), Some(expected.as_path()));
    }

    #[rstest::rstest]
    #[test]
    fn log_tail_lines_defaults_to_none() {
        // Given a temp-rooted --db-path and no --log-tail-lines argument.
        let dir = jinn_testutil::tempdir();
        let db_path = dir
            .path
            .join("test.db")
            .expect("temp root joins a bare name")
            .to_string();

        // When parsing.
        let cli = Cli::parse_from(["jinn", "--db-path", db_path.as_str()]);

        // Then Cli.log_tail_lines is None (the historical default applies).
        assert!(cli.log_tail_lines.is_none());
    }

    #[rstest::rstest]
    #[test]
    fn log_tail_lines_zero_parses_as_keep_everything() {
        // Given a temp-rooted --db-path and --log-tail-lines 0.
        let dir = jinn_testutil::tempdir();
        let db_path = dir
            .path
            .join("test.db")
            .expect("temp root joins a bare name")
            .to_string();

        // When parsing.
        let cli = Cli::parse_from([
            "jinn",
            "--db-path",
            db_path.as_str(),
            "--log-tail-lines",
            "0",
        ]);

        // Then the zero survives as Some(0) — absent and 0 are distinct
        // values, and 0 means keep everything.
        assert_eq!(cli.log_tail_lines, Some(0));
    }

    #[rstest::rstest]
    #[test]
    fn log_tail_lines_rejects_non_numeric_values() {
        // Given a temp-rooted --db-path and a non-numeric --log-tail-lines.
        let dir = jinn_testutil::tempdir();
        let db_path = dir
            .path
            .join("test.db")
            .expect("temp root joins a bare name")
            .to_string();

        // When parsing.
        let result = Cli::try_parse_from([
            "jinn",
            "--db-path",
            db_path.as_str(),
            "--log-tail-lines",
            "many",
        ]);

        // Then clap rejects it (usize value parsing).
        assert!(result.is_err());
    }

    #[rstest::rstest]
    #[test]
    fn log_dir_flag_removed() {
        // Given the old --log-dir argument (and a dummy --db-path, so a
        // missing --db-path cannot trigger first in debug mode).
        let dir = jinn_testutil::tempdir();
        let db_path = dir
            .path
            .join("test.db")
            .expect("temp root joins a bare name")
            .to_string();
        let log_dir = dir
            .path
            .join("logs")
            .expect("temp root joins a bare name")
            .to_string();

        // When parsing.
        let result = Cli::try_parse_from([
            "jinn",
            "--db-path",
            db_path.as_str(),
            "--log-dir",
            log_dir.as_str(),
        ]);

        // Then clap rejects it (the flag has been removed).
        assert!(result.is_err());
    }

    #[rstest::rstest]
    #[test]
    fn headless_scoped_log_file_removed() {
        // Given --log-file scoped to the headless subcommand (old shape).
        let dir = jinn_testutil::tempdir();
        let db_path = dir
            .path
            .join("test.db")
            .expect("temp root joins a bare name")
            .to_string();
        let log_file = dir
            .path
            .join("x.log")
            .expect("temp root joins a bare name")
            .to_string();

        // When parsing.
        let result = Cli::try_parse_from([
            "jinn",
            "--db-path",
            db_path.as_str(),
            "headless",
            "--log-file",
            log_file.as_str(),
            "send-chat",
            "hi",
        ]);

        // Then clap rejects it: the flag is global now, not a subcommand arg.
        assert!(result.is_err());
    }

    #[rstest::rstest]
    #[test]
    fn log_file_flag_works_with_headless() {
        // Given a global --log-file alongside a headless subcommand.
        let dir = jinn_testutil::tempdir();
        let expected = dir.path.join("x.log").expect("temp root joins a bare name");
        let log_file = expected.to_string_lossy().to_string();
        let db_path = dir
            .path
            .join("test.db")
            .expect("temp root joins a bare name")
            .to_string();

        // When parsing.
        let cli = Cli::parse_from([
            "jinn",
            "--db-path",
            db_path.as_str(),
            "--log-file",
            log_file.as_str(),
            "headless",
            "send-chat",
            "hi",
        ]);

        // Then Cli.log_file captures the override.
        assert_eq!(cli.log_file.as_deref(), Some(expected.as_path()));
    }

    #[rstest::rstest]
    #[test]
    fn config_flag_defaults_to_none() {
        // Given a temp-rooted --db-path and no --config argument.
        let dir = jinn_testutil::tempdir();
        let db_path = dir
            .path
            .join("test.db")
            .expect("temp root joins a bare name")
            .to_string();

        // When parsing.
        let cli = Cli::parse_from(["jinn", "--db-path", db_path.as_str()]);

        // Then Cli.config is None (the default location is used).
        assert!(cli.config.is_none());
    }

    #[rstest::rstest]
    #[test]
    fn config_flag_before_subcommand_captures_override() {
        // Given a --config argument before a subcommand.
        let dir = jinn_testutil::tempdir();
        let expected = dir
            .path
            .join("alt.toml")
            .expect("temp root joins a bare name");
        let config = expected.to_string_lossy().to_string();
        let db_path = dir
            .path
            .join("test.db")
            .expect("temp root joins a bare name")
            .to_string();

        // When parsing.
        let cli = Cli::parse_from([
            "jinn",
            "--db-path",
            db_path.as_str(),
            "--config",
            config.as_str(),
            "tui",
        ]);

        // Then Cli.config captures the override and the subcommand still parses.
        assert_eq!(cli.config.as_deref(), Some(expected.as_path()));
        // And the subcommand was still recognized.
        assert!(matches!(cli.command, Some(Commands::Tui)));
    }

    #[rstest::rstest]
    #[test]
    fn config_flag_after_subcommand_captures_override() {
        // Given a --config argument after a subcommand.
        let dir = jinn_testutil::tempdir();
        let expected = dir
            .path
            .join("alt.toml")
            .expect("temp root joins a bare name");
        let config = expected.to_string_lossy().to_string();
        let db_path = dir
            .path
            .join("test.db")
            .expect("temp root joins a bare name")
            .to_string();

        // When parsing.
        let cli = Cli::parse_from([
            "jinn",
            "--db-path",
            db_path.as_str(),
            "tui",
            "--config",
            config.as_str(),
        ]);

        // Then Cli.config captures the override (the flag is global).
        assert_eq!(cli.config.as_deref(), Some(expected.as_path()));
    }

    #[cfg(debug_assertions)]
    #[rstest::rstest]
    #[test]
    fn debug_guard_rejects_session_commands_without_db_path() {
        // Given no --db-path flag and a session-store command.
        // When parsing and running the debug guard.
        let cli = Cli::parse_from(["jinn", "tui"]);
        let result = cli.debug_db_path_required();

        // Then the guard rejects it (development must not touch production).
        assert!(result.is_err());
    }

    #[cfg(debug_assertions)]
    #[rstest::rstest]
    #[test]
    fn debug_guard_admits_mcp_subcommands_without_db_path() {
        // Given no --db-path flag and the bundled MCP server command.
        // When parsing and running the debug guard.
        let cli = Cli::parse_from(["jinn", "mcp", "overgraph"]);
        let result = cli.debug_db_path_required();

        // Then the guard passes: server children never open the session store.
        assert!(result.is_ok());
    }

    #[cfg(debug_assertions)]
    #[rstest::rstest]
    #[test]
    fn debug_guard_admits_remote_subcommands_without_db_path() {
        // Given no --db-path flag and the external remote client.
        // When parsing and running the debug guard.
        let cli = Cli::parse_from(["jinn", "remote", "instances"]);
        let result = cli.debug_db_path_required();

        // Then the guard passes: the client never opens the session store.
        assert!(result.is_ok());
    }

    #[rstest::rstest]
    #[test]
    fn remote_send_parses_with_an_explicit_mode() {
        // Given a send argv naming the mode and the session.
        // When parsing.
        let cli = Cli::parse_from([
            "jinn",
            "remote",
            "send",
            "--session",
            "abc",
            "--steer",
            "do the thing",
        ]);

        // Then the variant carries the mode and the message.
        let Some(Commands::Remote {
            subcommand:
                RemoteCommands::Send {
                    session,
                    queue,
                    steer,
                    instance,
                    message,
                },
        }) = cli.command
        else {
            panic!("remote send should parse");
        };
        assert_eq!(session, "abc");
        assert!(!queue);
        assert!(steer);
        assert_eq!(instance, None);
        assert_eq!(message, "do the thing");
    }

    #[rstest::rstest]
    #[test]
    fn remote_send_defaults_to_queue_when_no_mode_flag_is_given() {
        // Given a send argv with neither mode flag.
        // When parsing.
        let cli = Cli::parse_from(["jinn", "remote", "send", "--session", "abc", "hello"]);

        // Then neither flag is set; the client reads that as queue.
        let Some(Commands::Remote {
            subcommand: RemoteCommands::Send { queue, steer, .. },
        }) = cli.command
        else {
            panic!("remote send should parse");
        };
        assert!(!queue);
        assert!(!steer);
    }

    #[rstest::rstest]
    #[test]
    fn remote_send_rejects_both_mode_flags() {
        // Given a send argv carrying both --queue and --steer.
        // When parsing.
        let result = Cli::try_parse_from([
            "jinn",
            "remote",
            "send",
            "--session",
            "abc",
            "--queue",
            "--steer",
            "hello",
        ]);

        // Then parsing fails: the caller must pick one mode.
        assert!(result.is_err());
    }

    #[rstest::rstest]
    #[test]
    fn remote_send_requires_the_session_flag() {
        // Given a send argv with no --session.
        // When parsing.
        let result = Cli::try_parse_from(["jinn", "remote", "send", "hello"]);

        // Then parsing fails naming the missing flag.
        assert!(result.is_err());
    }

    #[rstest::rstest]
    #[test]
    fn remote_send_parses_an_instance_pid() {
        // Given a send argv targeting an explicit instance.
        // When parsing.
        let cli = Cli::parse_from([
            "jinn",
            "remote",
            "send",
            "--session",
            "abc",
            "--instance",
            "4242",
            "hello",
        ]);

        // Then the pid rides the variant.
        let Some(Commands::Remote {
            subcommand: RemoteCommands::Send { instance, .. },
        }) = cli.command
        else {
            panic!("remote send should parse");
        };
        assert_eq!(instance, Some(4242));
    }

    #[rstest::rstest]
    #[test]
    fn remote_instances_parses_bare() {
        // Given the listing argv.
        // When parsing.
        let cli = Cli::parse_from(["jinn", "remote", "instances"]);

        // Then the variant matches.
        assert!(matches!(
            cli.command,
            Some(Commands::Remote {
                subcommand: RemoteCommands::Instances
            })
        ));
    }

    #[cfg(debug_assertions)]
    #[rstest::rstest]
    #[test]
    fn debug_guard_passes_with_db_path_provided() {
        // Given --db-path and a session-store command. The value is never
        // read, only presence — a relative name keeps the test portable.
        let cli = Cli::parse_from(["jinn", "--db-path", "test-session-store.db", "tui"]);

        // When the debug guard is run.
        let result = cli.debug_db_path_required();

        // Then the guard passes.
        assert!(result.is_ok());
    }

    #[rstest::rstest]
    #[test]
    fn db_path_opt_returns_some_when_provided() {
        // Given a temp-rooted --db-path.
        let dir = jinn_testutil::tempdir();
        let expected = dir
            .path
            .join("test.db")
            .expect("temp root joins a bare name");
        let expected_str = expected.to_string_lossy().to_string();

        // When parsing.
        let cli = Cli::parse_from(["jinn", "--db-path", expected_str.as_str()]);

        // Then db_path_opt returns the path.
        assert_eq!(
            cli.db_path_opt().map(std::path::PathBuf::as_path),
            Some(expected.as_path())
        );
    }

    #[rstest::rstest]
    #[test]
    fn mcp_subcommand_is_named_overgraph() {
        // Given the builtin MCP server's argv shape.
        // When parsing.
        let cli = Cli::parse_from(["jinn", "mcp", "overgraph"]);

        // Then the variant matches.
        assert!(matches!(
            cli.command,
            Some(Commands::Mcp {
                subcommand: McpCommands::Overgraph { .. }
            })
        ));
    }

    #[rstest::rstest]
    #[test]
    fn dictation_list_devices_parses() {
        // Given the dictation list-devices argv shape.
        // When parsing.
        let cli = Cli::parse_from(["jinn", "dictation", "list-devices"]);

        // Then the variant matches.
        assert!(matches!(
            cli.command,
            Some(Commands::Dictation {
                subcommand: DictationCommands::ListDevices
            })
        ));
    }

    #[rstest::rstest]
    #[test]
    fn dictation_command_needs_no_db_path() {
        // Given a dictation command with no --db-path (it reads the
        // audio host, never the session store).
        let cli = Cli::parse_from(["jinn", "dictation", "list-devices"]);

        // When the debug-build guard runs.
        // Then it passes: dictation joins the MCP/remote exemption.
        cli.debug_db_path_required().expect("dictation is exempt");
    }
}
