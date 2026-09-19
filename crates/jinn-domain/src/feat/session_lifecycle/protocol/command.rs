//! Lifecycle command structs - async execution requests for setup/teardown.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{BusMessage, protocol::SessionId};

/// Request to run a lifecycle setup command asynchronously.
///
/// Sent by the `IntentHandler` when the user creates a session from a lifecycle
/// that has a `setup_command`. The session-persistence actor receives this,
/// runs the command via `run_lifecycle_command`, and updates the session state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunSessionSetup {
    /// The session this setup is for.
    pub session_id: SessionId,
    /// The shell command to execute (already rendered with args).
    pub command: String,
    /// Positional arguments for the command.
    pub args: Vec<String>,
    /// The original lifecycle command, used to dispatch builtin vs shell.
    /// `None` for backward compatibility with existing callers.
    #[serde(default)]
    pub lifecycle_command: Option<jinn_preferences_config::schemas::LifecycleCommand>,
}

impl BusMessage for RunSessionSetup {}

jinn_slices::crossing_schema!(RunSessionSetup, "RunSessionSetup",
trouper::schema::SchemaKind::Command,
description: "Run a session's setup lifecycle command.",
fields: ["session_id" => trouper::schema::FieldTy::Uuid,]);

/// Request to run a lifecycle teardown command asynchronously.
///
/// Sent by the sidebar teardown handler when the user triggers teardown-only
/// mode (`t` key). The session-persistence actor receives this, runs the command,
/// advances `lifecycle_script_state` to `TeardownRan`, persists, and emits
/// `SessionTeardownFinished`. The session is NOT removed from memory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunSessionTeardown {
    /// The session being torn down.
    pub session_id: SessionId,
    /// The shell command to execute (already rendered with args).
    pub command: String,
    /// Positional arguments (replayed from setup).
    pub args: Vec<String>,
}

impl BusMessage for RunSessionTeardown {}

jinn_slices::crossing_schema!(RunSessionTeardown, "RunSessionTeardown",
trouper::schema::SchemaKind::Command,
description: "Run a session's teardown lifecycle command.",
fields: ["session_id" => trouper::schema::FieldTy::Uuid,]);

/// Request to persist a session to SQLite immediately.
///
/// Emitted by the `IntentHandler` alongside `RunSessionSetup` so the session
/// is saved before the setup command even begins executing. This ensures the
/// session's lifecycle metadata (name, args) survives an app crash during setup.
/// Also used by other flows (teardown, archive) that need to persist state changes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistSession {
    /// The session to persist.
    pub session_id: SessionId,
}

impl crate::common::bus::BusMessage for PersistSession {}

jinn_slices::crossing_schema!(PersistSession, "PersistSession",
trouper::schema::SchemaKind::Command,
description: "Persist a session snapshot to the store.",
fields: ["session_id" => trouper::schema::FieldTy::Uuid,]);

/// Request to set a session's working directory.
///
/// Sent by the CWD input popup and CWD selector instead of mutating state
/// directly. The session-persistence actor applies the cwd and emits
/// `SessionCwdChanged`, which triggers re-discovery of skills, prompts, and
/// context files for the new cwd.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetSessionCwd {
    /// The session whose cwd is being set.
    pub session_id: SessionId,
    /// The new working directory.
    pub cwd: PathBuf,
}

impl crate::common::bus::BusMessage for SetSessionCwd {}

jinn_slices::crossing_schema!(SetSessionCwd, "SetSessionCwd",
trouper::schema::SchemaKind::Command,
description: "Set a session's working directory.",
fields: ["session_id" => trouper::schema::FieldTy::Uuid,]);

/// What the session actor does after a teardown finishes successfully.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum TeardownFollowUp {
    /// Teardown only: the session stays open (the sidebar `t` key).
    None,
    /// Close-with-teardown: archive/remove the session after teardown
    /// succeeds (the sidebar `x` key).
    Close,
    /// Teardown-tree: archive/remove the session and its whole subtree after
    /// teardown succeeds (the sidebar `X` key).
    CloseTree,
}

/// Result of an async teardown shell command, sent back to the session actor.
///
/// Emitted by the tokio task spawned during `handle_run_session_teardown` or
/// `handle_close_session`. The session actor processes this to advance lifecycle
/// state, emit events, and then perform the requested [`TeardownFollowUp`]
/// (keep open, close, or close the whole tree).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FinishSessionTeardown {
    /// The session that was being torn down.
    pub session_id: SessionId,
    /// What to do with the session (or its subtree) once teardown finishes.
    pub follow_up: TeardownFollowUp,
    /// Error message if teardown failed.
    pub error: Option<String>,
}

impl BusMessage for FinishSessionTeardown {}

jinn_slices::crossing_schema!(FinishSessionTeardown, "FinishSessionTeardown",
trouper::schema::SchemaKind::Event,
description: "A teardown lifecycle command finished.",
fields: ["session_id" => trouper::schema::FieldTy::Uuid,]);

/// Result of an async setup shell command, sent back to the session actor.
///
/// Emitted by the tokio task spawned during `handle_run_session_setup`.
/// The session actor processes this to set the CWD, advance lifecycle state,
/// and emit `SessionSetupCompleted`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FinishSessionSetup {
    /// The session that was being set up.
    pub session_id: SessionId,
    /// The CWD path on success, or `None` on error.
    pub cwd: Option<std::path::PathBuf>,
    /// Error message if setup failed.
    pub error: Option<String>,
}

impl BusMessage for FinishSessionSetup {}

jinn_slices::crossing_schema!(FinishSessionSetup, "FinishSessionSetup",
trouper::schema::SchemaKind::Event,
description: "A setup lifecycle command finished.",
fields: ["session_id" => trouper::schema::FieldTy::Uuid,]);

/// Request to cancel a running lifecycle command (setup or teardown).
///
/// Kills the spawned shell process if one is running, transitions
/// the session from `Working` back to `Idle`, and pushes a system entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelLifecycleCommand {
    /// The session whose lifecycle command should be cancelled.
    pub session_id: SessionId,
}

impl crate::common::bus::BusMessage for CancelLifecycleCommand {}

jinn_slices::crossing_schema!(CancelLifecycleCommand, "CancelLifecycleCommand",
trouper::schema::SchemaKind::Command,
description: "Cancel a running lifecycle shell command.",
fields: ["session_id" => trouper::schema::FieldTy::Uuid,]);
