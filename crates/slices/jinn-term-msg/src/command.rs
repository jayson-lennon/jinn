//! Request messages the coordinator actor answers.

use serde::{Deserialize, Serialize};

use jinn_core_types::SessionId;
use jinn_slices::BusMessage;

use crate::ExitInfo;

/// Who may send input to the session right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ControlHolder {
    /// The agent (tool calls) may send input.
    #[default]
    Agent,
    /// The user took over from the terminal tab; agent input is refused.
    User,
}

/// Outcome of a settle wait for a spawn or send.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TermScreen {
    /// The rendered screen (plain text, trailing blank rows trimmed).
    pub screen: String,
    /// Set when the process exited during (or before) this call.
    pub exited: Option<ExitInfo>,
}

/// Spawn a new interactive session running `command`.
///
/// One terminal per chat session: a spawn for a session that already has a
/// live terminal kills the old one first (reported in the outcome). The
/// terminal overlay and sidebar symbol are keyed by this chat session id —
/// it *is* the terminal's identity; there is no separate term id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnTerm {
    /// The chat session that owns this terminal.
    pub chat_session_id: SessionId,
    /// Shell command to run (passed to `bash -c`).
    pub command: String,
    /// Working directory for the child.
    pub cwd: std::path::PathBuf,
    /// Requested pty size (rows, cols).
    pub size: (u16, u16),
    /// How long to wait for output to settle before replying.
    pub max_wait: std::time::Duration,
}

/// Outcome of a [`SpawnTerm`] request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SpawnTermOutcome {
    /// Session created; screen captured after the initial settle.
    Started {
        /// The post-settle screen.
        screen: TermScreen,
        /// Set when this spawn killed the chat session's previous terminal —
        /// the caller must surface it so the agent knows the old program died.
        killed_previous: Option<KilledPrevious>,
    },
    /// The command failed to spawn (e.g. binary not found).
    Failed(String),
}

/// What happened to a chat session's previous terminal when a new one took
/// its place (one terminal per chat session).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KilledPrevious {
    /// Captured exit info from the kill.
    pub exited: ExitInfo,
}

/// Send input to a session and wait for the screen to settle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendTermInput {
    /// The chat session whose terminal receives the input.
    pub chat_session_id: SessionId,
    /// Verbatim text to type (`None` = don't type text).
    pub text: Option<String>,
    /// Named keys to press, in order.
    pub keys: Vec<String>,
    /// Whether to press enter after text/keys.
    pub enter: bool,
    /// How long to wait for output to settle before replying.
    pub max_wait: std::time::Duration,
}

/// Outcome of a [`SendTermInput`] request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SendTermOutcome {
    /// Input written; screen settled.
    Sent(TermScreen),
    /// The user holds control — nothing was written; no screen is returned
    /// (the user's terminal is theirs to read). The caller must fail the
    /// tool call with the wait notice.
    UserHasControl,
    /// The session id is unknown.
    UnknownSession,
    /// The session already exited; screen plus captured exit info.
    Exited(TermScreen),
}

/// Kill a session (its whole process group) and collect the final state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KillTerm {
    /// The chat session whose terminal is killed.
    pub chat_session_id: SessionId,
}

/// Outcome of a [`KillTerm`] request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum KillTermOutcome {
    /// The session was killed (or had already exited — kill is idempotent).
    Killed {
        /// Final rendered screen.
        screen: String,
        /// Transcript tail (sequence of observed screens).
        transcript_tail: String,
        /// Captured exit info.
        exited: ExitInfo,
    },
    /// The session id is unknown.
    UnknownSession,
}

impl BusMessage for SendTermKey {}

jinn_slices::crossing_schema!(SendTermKey, "SendTermKey",
trouper::schema::SchemaKind::Command,
description: "Forward one key event's bytes to a session's pty.",
fields: ["chat_session_id" => trouper::schema::FieldTy::Uuid]);

/// Resize a session's pty + emulator to the terminal overlay's inner rect.
///
/// Published by the render layer when the terminal overlay's inner rect
/// changes. Fire-and-forget; the actor clamps to sane bounds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResizeTerm {
    /// The chat session whose terminal resizes. `None` is a no-op (the
    /// render layer always names the active session; it never broadcasts).
    pub chat_session_id: Option<SessionId>,
    /// New size as `(rows, cols)`.
    pub size: (u16, u16),
}

impl BusMessage for ResizeTerm {}

jinn_slices::crossing_schema!(ResizeTerm, "ResizeTerm",
trouper::schema::SchemaKind::Command,
description: "Resize a session's pty and emulator.",
fields: ["chat_session_id" => trouper::schema::FieldTy::Uuid]);

/// Forward one key event's bytes to a chat session's pty (user control mode).
///
/// Fire-and-forget: keystrokes must not queue behind screen settle waits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendTermKey {
    /// The chat session whose terminal receives the bytes.
    pub chat_session_id: SessionId,
    /// Encoded key bytes to write.
    pub bytes: Vec<u8>,
}


impl BusMessage for SpawnTerm {}

jinn_slices::crossing_schema!(SpawnTerm, "SpawnTerm",
trouper::schema::SchemaKind::Command,
description: "Spawn an interactive pty session for a chat session.",
fields: ["chat_session_id" => trouper::schema::FieldTy::Uuid, "command" => trouper::schema::FieldTy::Str]);

impl BusMessage for SpawnTermOutcome {}

jinn_slices::crossing_schema!(SpawnTermOutcome, "SpawnTermOutcome",
trouper::schema::SchemaKind::Event,
description: "Reply payload for the spawn ask.",
fields: []);

impl BusMessage for KilledPrevious {}

jinn_slices::crossing_schema!(KilledPrevious, "KilledPrevious",
trouper::schema::SchemaKind::Event,
description: "The replaced terminal's exit info.",
fields: []);

impl BusMessage for SendTermInput {}

jinn_slices::crossing_schema!(SendTermInput, "SendTermInput",
trouper::schema::SchemaKind::Command,
description: "Send text/keys to a session's pty and wait for settle.",
fields: ["chat_session_id" => trouper::schema::FieldTy::Uuid]);

impl BusMessage for SendTermOutcome {}

jinn_slices::crossing_schema!(SendTermOutcome, "SendTermOutcome",
trouper::schema::SchemaKind::Event,
description: "Reply payload for the send-input ask.",
fields: []);

impl BusMessage for KillTerm {}

jinn_slices::crossing_schema!(KillTerm, "KillTerm",
trouper::schema::SchemaKind::Command,
description: "Kill a chat session's pty process group.",
fields: ["chat_session_id" => trouper::schema::FieldTy::Uuid]);

impl BusMessage for KillTermOutcome {}

jinn_slices::crossing_schema!(KillTermOutcome, "KillTermOutcome",
trouper::schema::SchemaKind::Event,
description: "Reply payload for the kill ask.",
fields: []);

impl BusMessage for TermScreen {}

jinn_slices::crossing_schema!(TermScreen, "TermScreen",
trouper::schema::SchemaKind::Event,
description: "A settled terminal screen capture.",
fields: ["screen" => trouper::schema::FieldTy::Str]);
