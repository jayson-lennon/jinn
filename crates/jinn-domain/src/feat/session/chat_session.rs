//! Chat session protocol - state types for a single conversation.
//!
//! [`ChatSessionState`] owns the history and streaming state for one chat session.
//! Multiple sessions can exist concurrently in the application, each identified
//! by a [`SessionId`](crate::protocol::SessionId).
//!
//! Fields are grouped into [`SessionCore`] (session-actor / context-actor)
//! and [`SessionUi`] (IntentHandler) sub-structs to make cross-boundary
//! writes visually obvious during code review.

#![expect(
    clippy::partial_pub_fields,
    clippy::field_scoped_visibility_modifiers,
    reason = "ChatSessionState uses scoped visibility on `core` to enforce the capsule wall: \
        the field is private to the session subtree so cross-actor reach-throughs cannot compile, \
        while `ui` stays pub for IntentHandler. Mixed pub/scoped visibility is intentional."
)]

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::atomic::Ordering;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::feat::session::chat_history::ChatHistory;
use crate::feat::session::history_editor::HistoryEditor;
use crate::feat::session::phase_machine::PhaseKind;
use crate::feat::session::profile::SessionProfile;
use crate::feat::session::steering_buffer::SteeringBuffer;
use crate::feat::session::token_stats::TokenRecord;
use crate::feat::ui::chat_log::visual_item::VisualItem;
use crate::protocol::{
    ChangeSource, ChatEntry, ChatEntryId, ChatEntryKind, ContextOverride, PinPosition, SessionId,
};
use jinn_core_types::model_selection::ModelSelection;

use crate::feat::context::prompt_template::PromptTemplateStore;
use crate::feat::context::prompt_template::{PathResolveContext, PendingPath};
use crate::feat::context::prompt_template::{expand_tokens, scan_at_paths_with_degraded};
use crate::feat::session::entry_timing::EntryTiming;

/// Error returned when a streaming operation fails.
#[derive(Debug, wherror::Error)]
pub enum StreamingError {
    /// No streaming entry index is set.
    #[error("no streaming entry index")]
    NoStreamingEntry,
    /// The streaming entry has an unexpected kind.
    #[error("streaming entry is not an Assistant entry")]
    NotAssistantEntry,
    /// No thinking entry index is set.
    #[error("no thinking entry index")]
    NoThinkingEntry,
    /// No tool call tracked for the given stream index.
    #[error("no entry tracked for tool call stream index {index}")]
    NoToolCallIndex { index: usize },
    /// The token ledger is empty.
    #[error("token ledger is empty")]
    EmptyLedger,
}

/// Whether a session is in memory or at rest in the database.
///
/// `Loaded` sessions appear in the sidebar and are available for interaction.
/// `Archived` sessions exist only in the database and are hidden from the sidebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    #[default]
    Loaded,
    Archived,
}

/// The lifecycle script progression for a session.
///
/// One-way transitions enforced by [`advance_after_setup`](Self::advance_after_setup)
/// and [`advance_after_teardown`](Self::advance_after_teardown).
/// These methods are only called after the corresponding script succeeds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleScriptState {
    #[default]
    NothingRan,
    SetupRan,
    TeardownRan,
}

impl LifecycleScriptState {
    /// Transition `NothingRan → SetupRan`.
    ///
    /// Soft guard: if current state is not `NothingRan`, logs a warning and returns.
    pub fn advance_after_setup(&mut self) {
        if !matches!(self, Self::NothingRan) {
            tracing::warn!(current = ?self, "advance_after_setup: expected NothingRan, ignoring");
            return;
        }
        *self = Self::SetupRan;
    }

    /// Transition `SetupRan → TeardownRan`.
    ///
    /// Soft guard: if current state is not `SetupRan`, logs a warning and returns.
    pub fn advance_after_teardown(&mut self) {
        if !matches!(self, Self::SetupRan) {
            tracing::warn!(current = ?self, "advance_after_teardown: expected SetupRan, ignoring");
            return;
        }
        *self = Self::TeardownRan;
    }
}

impl std::fmt::Display for LifecycleScriptState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::NothingRan => "nothing_ran",
            Self::SetupRan => "setup_ran",
            Self::TeardownRan => "teardown_ran",
        };
        f.write_str(s)
    }
}

/// Groups runtime-only fields that are specific to the current running instance
/// and have no meaning across restarts (stream indices, queues, in-progress flags).
/// The entire struct is skipped during serialization so individual fields cannot
/// be accidentally excluded from persistence.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionCoreEphemeral {
    /// Validated phase transition machine.
    /// The single source of truth for phase state.
    pub machine: crate::feat::session::phase_machine::SessionPhaseMachine,
    /// Turn dispatch queue - drives all turn transitions through a single processor.
    pub message_queue: crate::feat::session::turn_queue::TurnQueue,
    /// Cached context size in tokens (assembled prompt size).
    /// Updated when context is assembled. Not persisted across restarts.
    /// OWNER: session-actor.
    pub cached_context_size: Option<u32>,

    /// Number of active background operations (e.g., lifecycle tasks).
    /// Ephemeral: not persisted, not serialized.
    /// OWNER: session-actor.
    #[serde(skip)]
    pub busy_count: usize,
    /// `dispatched_at` of the currently in-flight stream generation.
    ///
    /// Set when a turn is dispatched (`begin_streaming`) and compared on
    /// `StreamCompleted` to reject stale terminal events from an aborted prior
    /// stream (e.g. a retry re-dispatched while the old task was still alive).
    /// A completion whose `dispatched_at` is older than this value belongs to a
    /// superseded generation and is dropped. OWNER: session-actor.
    #[serde(skip)]
    pub stream_dispatched_at: Option<Timestamp>,

    /// Accumulated context-override mutations held back until their deduplicated
    /// token total crosses the accumulation threshold.
    ///
    /// Only non-compaction `SetContextOverride` mutations from auto-pruners are
    /// buffered here; pins/inserts and compaction overrides take the immediate
    /// `pending_mutations` path. OWNER: session-actor. Not persisted.
    #[serde(skip)]
    pub accumulated_overrides: crate::feat::session::mutation_accumulator::MutationAccumulator,

    /// Pending history mutation batches from background workers.
    /// Drained and applied at safe application points (tool batch completion,
    /// stream completion). Not persisted across restarts.
    #[serde(skip)]
    pub pending_mutations: Vec<Vec<crate::feat::session::history_mutation::HistoryMutation>>,

    /// Discovered resources for THIS session, scoped to its cwd tree.
    /// Populated by the scan actors (skills / prompts / context-files).
    /// Ephemeral: not persisted, re-scanned from disk on session load.
    /// OWNER: the session-init slice's discovery worker.
    /// See `.plans/project-locals/plan.md` decision D3 — per-session isolation.
    #[serde(skip)]
    pub discovered_skills: Vec<crate::feat::skills::Skill>,

    /// Discovered prompt templates for this session (merged global + project).
    /// OWNER: the session-init slice's discovery worker.
    #[serde(skip)]
    pub discovered_prompt_templates: crate::feat::context::prompt_template::PromptTemplateStore,

    /// Discovered AGENTS.md/CLAUDE.md context files for this session, ordered
    /// root-first (root ancestor first, cwd last) for prompt assembly.
    /// OWNER: context-files scan actor.
    #[serde(skip)]
    pub discovered_context_files: Vec<crate::feat::context::env_context::ContextFile>,

    /// Buffered `ToolBatchCompleted` results that arrived while the session
    /// was still `Streaming` (i.e. the `StreamCompleted(ToolUse)` event that
    /// transitions `Streaming → Sending` was still in flight on the bus).
    ///
    /// Drained by `on_stream_completed` once the matching `StreamCompleted(ToolUse)`
    /// transitions the session to `Sending`, so the continuation is dispatched
    /// instead of the batch being dropped as stale. OWNER: session-actor.
    #[serde(skip)]
    pub pending_tool_batch: Option<Vec<jinn_core_types::tool_types::ToolResult>>,
}

// Core session state - owned by session-actor and context-actor.
//
// IntentHandler is exempt and may read/write any field.
// No other actor should mutate these fields.
//
// Fields without `#[serde(skip)]` are persisted across restarts.
// All ephemeral (non-persisted) state lives in [`SessionCoreEphemeral`].

/// Serde default for the `cwd` field - resolves to the current directory.
fn default_cwd() -> std::path::PathBuf {
    std::path::PathBuf::from(".")
}

/// Serde default for [`SessionCore::persist`] — sessions persist unless explicitly marked transient.
pub fn default_persist() -> bool {
    true
}

/// How a session came into being. Identity, not structure: a session's
/// place in the tree is [`SessionCore::parent_session`]; its kind is
/// this enum.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionOrigin {
    /// Created by the user (new session, dashboard, restart restore of one).
    #[default]
    User,
    /// Created by forking an existing session at an ordinal.
    Fork,
    /// Spawned by the `task` tool as a child of another session.
    Subagent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionCore {
    /// Unique identifier for this session.
    /// Generated at construction. Matches the HashMap key in `SessionState.sessions`.
    pub session_id: SessionId,
    /// Human-readable title. `None` until the first user message is sent.
    /// OWNER: session-actor (set on first user message, changeable by user).
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// When this session was last updated. Set at construction, updated on save.
    pub updated_at: Timestamp,
    /// Wall-clock timestamp of the most recent chat-history mutation
    /// (entry pushed, stream token appended, thinking token appended).
    /// The stall watchdog compares `now - last_history_activity_at` against
    /// the stall timeout to detect hung sessions. Seeded on phase entry
    /// (`begin_sending`/`begin_streaming`) so the HTTP-handshake gap is covered.
    /// Runtime-only turn state — not persisted (a loaded session is `Idle`).
    /// OWNER: session-actor.
    #[serde(skip)]
    pub last_history_activity_at: Timestamp,
    /// Wall-clock timestamp of the most recent **provider output** (assistant
    /// text, thinking text, or tool-call deltas from the model) — the genuine
    /// "the provider is responsive" signal. Unlike `last_history_activity_at`,
    /// this is bumped only by provider-output methods, not by retry markers or
    /// phase transitions. The stall watchdog resets a session's retry budget
    /// when this advances between ticks, so a responsive provider that suffers
    /// intermittent contention stalls is not prematurely cancelled.
    /// Runtime-only turn state — not persisted.
    /// OWNER: session-actor.
    #[serde(skip)]
    pub last_provider_activity_at: Timestamp,
    /// When this session was created. Set once at construction, never mutated.
    pub created_at: Timestamp,
    /// All messages in this conversation.
    ///
    /// OWNER: history-editor (the sole write path; reads via
    /// [`ChatSessionState::history`]). Restoring from persistence is the one
    /// exception, performed by [`ChatSessionState::restore_history`] before
    /// the session becomes live.
    pub(in crate::feat::session) history: ChatHistory,
    /// Per-session model and strategy selection.
    /// OWNER: provider-actor (model), context-actor (strategy via SwitchPromptStrategy command)
    pub profile: SessionProfile,
    /// Working directory for tool execution in this session.
    /// OWNER: IntentHandler (set on session creation and cd commands)
    #[serde(default = "default_cwd")]
    pub cwd: std::path::PathBuf,
    /// User home directory for resolving `@~/path` references in this session.
    /// Runtime-only — not persisted (resolved fresh at session creation from
    /// `services.paths.home_dir()`).
    /// OWNER: IntentHandler / session creation.
    #[serde(skip)]
    pub home: std::path::PathBuf,
    /// Token usage ledger - one immutable record per request/response pair.
    /// OWNER: session-actor (records tokens on assembly and StreamCompleted).
    #[serde(default)]
    pub token_ledger: Vec<TokenRecord>,
    /// Parent session ID, if this session was forked from another.
    /// `None` means this is a root session.
    /// OWNER: session-actor (set at session creation).
    #[serde(default)]
    pub parent_session: Option<SessionId>,
    /// Highest entry ordinal inherited from the parent at fork time.
    /// `None` for root sessions (all entries are "own").
    /// `Some(n)` means entries at indices 0..=n were inherited;
    /// only entries after index n count as turns for this session.
    /// Set once at fork creation, never mutated.
    /// OWNER: session-actor (set during fork).
    #[serde(default)]
    pub fork_ordinal: Option<usize>,
    /// Identity of this session's creation path. Set at construction by the
    /// creating path (`new_child` → [`SessionOrigin::Subagent`], fork →
    /// [`SessionOrigin::Fork`]); never mutated afterwards.
    /// OWNER: session-actor (set at session creation).
    #[serde(default)]
    pub origin: SessionOrigin,
    /// Project directory this session is associated with. Stamped once at
    /// session creation from the projects UI; never follows later `cwd`
    /// changes. `None` means the session has no project association.
    /// OWNER: IntentHandler (set at session creation).
    #[serde(default)]
    pub project: Option<std::path::PathBuf>,

    /// Generic blob storage for future subsystems.
    #[serde(default)]
    pub blobs: HashMap<String, JsonValue>,
    /// Name of the session lifecycle that created this session.
    /// `None` means the implicit "blank" lifecycle (no setup command).
    /// OWNER: IntentHandler (set on session creation).
    #[serde(default)]
    pub lifecycle_name: Option<String>,
    /// Arguments passed to the lifecycle setup command.
    /// Replayed during teardown so the same args are available.
    /// OWNER: IntentHandler (set on session creation).
    #[serde(default)]
    pub lifecycle_args: Vec<String>,
    /// Whether this session is loaded in memory or archived in the database.
    /// OWNER: session-actor (transitions on close/archive/unarchive).
    #[serde(default)]
    pub session_state: SessionState,
    /// Lifecycle script progression - one-way: NothingRan → SetupRan → TeardownRan.
    /// OWNER: session-actor (advances only after script success).
    #[serde(default)]
    pub lifecycle_script_state: LifecycleScriptState,

    /// Whether this session should be persisted to disk. Default true; set
    /// false for transient automated sessions (e.g. one-shots).
    /// OWNER: session-actor (set on creation).
    #[serde(default = "default_persist")]
    pub persist: bool,

    /// Whether the user has meaningfully interacted with this session.
    /// Sessions with `has_interacted = false` are not persisted to disk.
    /// OWNER: session-actor (set via MarkSessionInteracted command).
    #[serde(default)]
    pub has_interacted: bool,
    /// Phased task list for agent session planning.
    /// OWNER: tools-actor (mutated by task list tools).
    #[serde(default)]
    pub task_list: jinn_tools_msg::TaskList,
    /// Names of MCP servers (`jinn.toml` `[[mcp_server]].name`) enabled for
    /// this session. Off by default — enabling spawns a dedicated `McpActor`
    /// and its child-process connection; disabling kills both. Persisted with
    /// the session.
    /// OWNER: IntentHandler (toggled via the MCP picker); McpCoordinatorActor
    /// reacts to the resulting commands.
    #[serde(default)]
    pub enabled_mcp_servers: std::collections::BTreeSet<String>,
    /// Live connection status of each enabled MCP server, keyed by server
    /// name. Runtime-only (derived from `McpServerStatus` actor events); not
    /// persisted.
    /// OWNER: McpCoordinatorActor (writes on `McpServerStatus` events).
    #[serde(skip)]
    pub mcp_server_status: std::collections::BTreeMap<String, jinn_mcp_msg::McpConnectionStatus>,
    /// Per-session captured stderr tail for each MCP server, updated live
    /// by the stderr-debounce republish.
    ///
    /// OWNER: McpCoordinatorActor (writes on `McpServerLog` events).
    #[serde(skip)]
    pub mcp_server_stderr: std::collections::BTreeMap<String, String>,
    /// Runtime-only state - not persisted across restarts.
    #[serde(skip)]
    pub ephemeral: SessionCoreEphemeral,
}

impl Default for SessionCore {
    fn default() -> Self {
        Self {
            session_id: SessionId::new(),
            title: None,
            updated_at: Timestamp::now(),
            created_at: Timestamp::now(),
            last_history_activity_at: Timestamp::now(),
            last_provider_activity_at: Timestamp::now(),
            history: ChatHistory::new(),
            profile: SessionProfile::default(),
            cwd: std::path::PathBuf::from("."),
            home: std::path::PathBuf::from("."),
            token_ledger: Vec::new(),
            parent_session: None,
            fork_ordinal: None,
            origin: SessionOrigin::User,
            project: None,

            blobs: HashMap::new(),
            lifecycle_name: None,
            lifecycle_args: Vec::new(),
            session_state: SessionState::Loaded,
            lifecycle_script_state: LifecycleScriptState::NothingRan,
            persist: true,

            task_list: jinn_tools_msg::TaskList::default(),
            enabled_mcp_servers: std::collections::BTreeSet::new(),
            mcp_server_status: std::collections::BTreeMap::new(),
            mcp_server_stderr: std::collections::BTreeMap::new(),
            has_interacted: false,
            ephemeral: SessionCoreEphemeral::default(),
        }
    }
}

// Re-export shim: `SavedHistoryPosition` moved to `jinn-slices` (part of
// the chat-log view vocabulary persisted in the chat-log-view slice's
// cell); the kernel path stays stable for consumers.
pub use jinn_chat_log_view_msg::SavedHistoryPosition;

/// UI state for a session - owned by IntentHandler (exempt from ownership restrictions).
///
/// These fields control visual presentation: the in-progress input text and
/// the steering buffer. The per-session chat log *view* state (scroll,
/// selection, expand/ignore sets, pins position, render caches) lives in the
/// chat-log-view slice's cell, reached through [`ChatSessionState`]'s view
/// facade.
#[derive(Debug, Default)]
pub struct SessionUi {
    /// In-memory steering buffer for this session.
    ///
    /// Accumulates user-submitted text fragments that will be drained
    /// into a single `User` chat entry at the next prompt-assembly
    /// boundary. Not serialized - `SessionUi` itself is in-memory only,
    /// so this field is dropped on session close.
    pub steering_buffer: SteeringBuffer,
}

impl Clone for SessionUi {
    fn clone(&self) -> Self {
        Self {
            steering_buffer: self.steering_buffer.clone(),
        }
    }
}

/// The state of a single chat session.
///
/// Owns the conversation history and tracks whether an LLM response is
/// currently streaming in. The streaming entry is an in-progress `Assistant`
/// entry at a known index - tokens are appended to it until the stream
/// completes or is cancelled.
///
/// Fields are grouped into [`SessionCore`] (session-actor / context-actor)
/// and [`SessionUi`] (IntentHandler) sub-structs to make cross-boundary
/// writes visually obvious during code review.
#[derive(Debug, Serialize, Deserialize)]
pub struct ChatSessionState {
    /// Core domain state managed by session-actor and context-actor.
    #[serde(flatten)]
    pub(in crate::feat::session) core: SessionCore,
    /// UI state managed by IntentHandler.
    #[serde(skip)]
    pub ui: SessionUi,
    /// Late-attached handle to the slice registry, carrying the slice
    /// cells (this session's display state, input draft, and any later
    /// per-session slices). Attached once at wiring; a clone of `Slices`
    /// shares its cells. Before attach (or without a slice's
    /// `activate()`), the facades fall back to the in-struct fallbacks —
    /// the removability property. (Composition attaches once on the
    /// session map; direct pokes defeat the facade.)
    #[serde(skip)]
    pub(in crate::feat::session) slices: std::sync::OnceLock<jinn_slices::Slices>,
    /// In-struct stand-in for this session's view state while
    /// `slices` is unattached. Reads see it, writes mutate it, so an
    /// unattached configuration behaves exactly like the pre-slice layout.
    /// Ignored entirely once the handle is attached.
    #[serde(skip)]
    pub(in crate::feat::session) view_fallback:
        parking_lot::RwLock<jinn_chat_log_view_msg::ChatLogViewUi>,
    /// In-struct stand-in for this session's input draft while `slices`
    /// is unattached. Same contract as [`Self::view_fallback`].
    #[serde(skip)]
    pub(in crate::feat::session) input_fallback:
        parking_lot::RwLock<jinn_chat_input_msg::ChatInputBoxState>,
}

impl Clone for ChatSessionState {
    fn clone(&self) -> Self {
        Self {
            core: self.core.clone(),
            ui: self.ui.clone(),
            // The clone does not inherit the registry handle: attachment
            // happens once per session at wiring. A cloned session's facade
            // runs on the fallback until (re)attached, which keeps
            // test-constructed sessions in the pre-slice configuration.
            slices: std::sync::OnceLock::new(),
            view_fallback: parking_lot::RwLock::new(self.view_fallback.read().clone()),
            input_fallback: parking_lot::RwLock::new(self.input_fallback.read().clone()),
        }
    }
}

impl ChatSessionState {
    /// Create a new session with empty history and no active stream.
    #[must_use]
    pub fn new() -> Self {
        Self {
            core: SessionCore::default(),
            ui: SessionUi::default(),
            slices: std::sync::OnceLock::new(),
            view_fallback: parking_lot::RwLock::new(
                jinn_chat_log_view_msg::ChatLogViewUi::default(),
            ),
            input_fallback: parking_lot::RwLock::new(jinn_chat_input_msg::ChatInputBoxState::new()),
        }
    }

    /// Opens the sole write path to this session's history.
    ///
    /// All history mutations go through the returned [`HistoryEditor`]; reads
    /// stay on the session itself.
    pub fn edit_history(&mut self) -> HistoryEditor<'_> {
        HistoryEditor::new(self)
    }

    /// Attaches the slice registry handle carrying this session's
    /// chat-log-view entry. Called once at wiring; later calls are ignored.
    pub fn attach_slices(&self, slices: jinn_slices::Slices) {
        let _ = self.slices.set(slices);
    }

    /// The session's chat-log-view cell, if the handle is attached and the
    /// slice's `activate()` minted the cell.
    fn view_cell(
        &self,
    ) -> Option<jinn_slices::cell::TypedCell<jinn_chat_log_view_msg::ChatLogViews>> {
        let slices = self.slices.get()?;
        slices.reader::<jinn_chat_log_view_msg::ChatLogViews>(
            &jinn_chat_log_view_msg::chat_log_views_slot(),
        )
    }

    /// The session's chat-input cell, if the handle is attached and the
    /// slice's `activate()` minted the cell.
    fn input_cell(&self) -> Option<jinn_slices::cell::TypedCell<jinn_chat_input_msg::ChatInputs>> {
        let slices = self.slices.get()?;
        slices.reader::<jinn_chat_input_msg::ChatInputs>(&jinn_chat_input_msg::chat_inputs_slot())
    }

    /// Runs `f` against this session's input draft (buffer, cursor, wrap
    /// cache, submission mode, autocomplete), keyed by the session id.
    /// Writers get-or-insert their session's entry; falls back to the
    /// in-struct draft when the cell is absent (handle unattached or slice
    /// not activated).
    pub fn update_input<F>(&self, f: F)
    where
        F: FnOnce(&mut jinn_chat_input_msg::ChatInputBoxState),
    {
        match self.input_cell() {
            Some(cell) => {
                let id = self.session_id().clone();
                cell.update(|inputs| f(inputs.entry(id).or_default()));
            }
            None => {
                let mut input = self.input_fallback.write();
                f(&mut input);
            }
        }
    }

    /// Reads this session's input draft through `f`, falling back to
    /// `default` when the cell is absent (handle unattached or slice not
    /// activated). Readers never grow the map: a session with no entry
    /// reads as its default draft.
    pub fn with_input<R, F, D>(&self, f: F, default: D) -> R
    where
        F: FnOnce(&jinn_chat_input_msg::ChatInputBoxState) -> R,
        D: FnOnce() -> R,
    {
        match self.input_cell() {
            Some(cell) => {
                let inputs = cell.read();
                let id = self.session_id();
                match inputs.get(id) {
                    Some(input) => f(input),
                    None => default(),
                }
            }
            None => {
                let input = self.input_fallback.read();
                f(&input)
            }
        }
    }

    /// Runs `f` against this session's view state (scroll, selection,
    /// expand/ignore sets, pins position, render caches), keyed by the
    /// session id. Writers get-or-insert their session's entry; a no-op
    /// when the cell is absent (handle unattached or slice not activated).
    pub fn update_view<F>(&self, f: F)
    where
        F: FnOnce(&mut jinn_chat_log_view_msg::ChatLogViewUi),
    {
        match self.view_cell() {
            Some(cell) => {
                let id = self.session_id().clone();
                cell.update(|views| f(views.entry(id).or_default()));
            }
            None => {
                let mut view = self.view_fallback.write();
                f(&mut view);
            }
        }
    }

    /// Reads this session's view state through `f`, falling back to
    /// `default` when the cell is absent (handle unattached or slice not
    /// activated). Readers never grow the map: a session with no entry
    /// reads as its default view.
    pub fn with_view<R, F, D>(&self, f: F, default: D) -> R
    where
        F: FnOnce(&jinn_chat_log_view_msg::ChatLogViewUi) -> R,
        D: FnOnce() -> R,
    {
        match self.view_cell() {
            Some(cell) => {
                let views = cell.read();
                let id = self.session_id();
                match views.get(id) {
                    Some(view) => f(view),
                    None => default(),
                }
            }
            None => {
                let view = self.view_fallback.read();
                f(&view)
            }
        }
    }

    /// Take-style mutation for view fields whose semantics consume the old
    /// value (the ignore-sweep). Returns what `f` removed.
    fn update_view_taking<R, F>(&self, f: F) -> Option<R>
    where
        R: Send + 'static,
        F: FnOnce(&mut jinn_chat_log_view_msg::ChatLogViewUi) -> Option<R>,
    {
        let taken = parking_lot::Mutex::new(None);
        self.update_view(|v| *taken.lock() = f(v));
        taken.into_inner()
    }

    /// A snapshot copy of the shown-ignored-blocks set. Builders that read
    /// the set alongside history (`build_visual_items`, sweep propagation)
    /// work on the copy so the view lock is never held across computation.
    #[must_use]
    pub fn shown_ignored_blocks_snapshot(&self) -> std::collections::HashSet<ChatEntryId> {
        self.with_view(|v| v.shown_ignored_blocks.clone(), Default::default)
    }

    /// Raw tail push used by the history editor. Applies user-entry token
    /// expansion and cursor/scroll bookkeeping. Do not call directly.
    pub(in crate::feat::session) fn push_entry_raw(&mut self, entry: &mut ChatEntry) -> usize {
        self.core.last_history_activity_at = Timestamp::now();
        let ctx = PathResolveContext::new(&self.core.cwd, &self.core.home);
        expand_user_entry(
            entry,
            &self.core.ephemeral.discovered_prompt_templates,
            &ctx,
        );
        let cursor_at_last = self.with_view(
            |v| {
                v.selected_cursor_id
                    .as_ref()
                    .is_none_or(|id| self.core.history.last().is_some_and(|e| &e.id == id))
            },
            || true,
        );
        let index = self.core.history.len();
        self.core.history.push(entry.clone());
        if cursor_at_last {
            self.reset_scroll();
            if let Some(entry) = self.core.history.last() {
                let id = entry.id.clone();
                self.update_view(|v| v.selected_cursor_id = Some(id));
            }
        }
        index
    }

    /// Removes the history entry at `index`. Returns whether it existed.
    ///
    /// Editor-only. Callers must remove in descending index order.
    pub(in crate::feat::session) fn remove_history_entry_at(&mut self, index: usize) -> bool {
        if index < self.core.history.len() {
            self.core.history.remove(index);
            true
        } else {
            false
        }
    }

    /// Mutable access to the history entry at `index` for the editor.
    ///
    /// In-place writes (streaming lifecycle) can never reorder entries or
    /// split a tool loop, so the editor exposes them without chunk logic.
    pub(in crate::feat::session) fn history_get_mut(
        &mut self,
        index: usize,
    ) -> Option<&mut ChatEntry> {
        self.core.history.get_mut(index)
    }

    /// Runs `f` on the entry with `id`, if it exists. Returns `f`'s output.
    ///
    /// Editor-only in-primitive for id-keyed in-place mutation.
    pub(in crate::feat::session) fn with_history_entry_mut<R>(
        &mut self,
        id: &ChatEntryId,
        f: impl FnOnce(&mut ChatEntry) -> R,
    ) -> Option<R> {
        self.core
            .history
            .iter_mut()
            .find(|entry| &entry.id == id)
            .map(f)
    }

    /// Create a new session with a specific profile (model + strategy).
    #[must_use]
    pub fn new_with_profile(profile: SessionProfile) -> Self {
        Self {
            core: SessionCore {
                profile,
                ..SessionCore::default()
            },
            ui: SessionUi::default(),
            slices: std::sync::OnceLock::new(),
            view_fallback: parking_lot::RwLock::new(
                jinn_chat_log_view_msg::ChatLogViewUi::default(),
            ),
            input_fallback: parking_lot::RwLock::new(jinn_chat_input_msg::ChatInputBoxState::new()),
        }
    }

    /// Create a child session: a fresh (empty-history) session linked to a parent.
    ///
    /// Sets `parent_session` and `persist`. The caller is responsible for
    /// inheriting the parent's model (via [`set_model`](Self::set_model))
    /// if desired - this constructor does not perform any state reads.
    /// The caller is also responsible for inheriting the parent's project
    /// stamp (via [`set_project`](Self::set_project), as `build_child` in the
    /// task tool does) - subagent sessions carry the project association of
    /// the session that spawned them.
    #[must_use]
    pub fn new_child(parent_session_id: &SessionId, persist: bool) -> Self {
        Self {
            core: SessionCore {
                parent_session: Some(parent_session_id.clone()),
                origin: SessionOrigin::Subagent,
                persist,
                ..SessionCore::default()
            },
            ui: SessionUi::default(),
            slices: std::sync::OnceLock::new(),
            view_fallback: parking_lot::RwLock::new(
                jinn_chat_log_view_msg::ChatLogViewUi::default(),
            ),
            input_fallback: parking_lot::RwLock::new(jinn_chat_input_msg::ChatInputBoxState::new()),
        }
    }

    /// Immutable access to this session's steering buffer.
    pub fn steering_buffer(&self) -> &SteeringBuffer {
        &self.ui.steering_buffer
    }
    /// Mutable access to this session's steering buffer.
    pub fn steering_buffer_mut(&mut self) -> &mut SteeringBuffer {
        &mut self.ui.steering_buffer
    }

    /// The session's persona name.
    pub fn persona_name(&self) -> &str {
        &self.core.profile.persona_name
    }

    /// Set the session's persona name.
    pub fn set_persona_name(&mut self, name: String) {
        self.core.profile.persona_name = name;
    }

    /// Read-only access to the conversation history.
    pub fn history(&self) -> &[ChatEntry] {
        &self.core.history
    }

    /// Fills the persisted token count for entries that don't have one yet.
    ///
    /// A token count is a content-derived fact about the (immutable) entry
    /// text, so entries whose count is already `Some` are never recomputed.
    /// Entry ids not present in `counts` (or not in this history) are skipped.
    ///
    /// Returns the number of entries filled.
    pub fn fill_missing_token_counts(&mut self, counts: &HashMap<ChatEntryId, u32>) -> usize {
        let mut filled = 0;
        for entry in self.core.history.iter_mut() {
            if entry.token_count.is_some() {
                continue;
            }
            if let Some(count) = counts.get(&entry.id) {
                entry.token_count = Some(*count);
                filled += 1;
            }
        }
        filled
    }

    /// Mark entries at the given indices as ignored.
    ///
    /// Used by the compaction actor to mark entries that have been summarized.
    /// Ignores any indices that are out of bounds.
    pub fn mark_entries_ignored(&mut self, indices: &[usize]) {
        for &i in indices {
            if let Some(entry) = self.core.history.get_mut(i) {
                entry.apply_context_override(
                    ContextOverride::ForcedExclude,
                    ChangeSource::Internal {
                        label: "mark_entries_ignored".into(),
                    },
                );
            }
        }
    }

    /// Toggle the context override on the currently selected entry, always
    /// flipping the entry's *effective* in-context state.
    ///
    /// `Forced*` states flip between include and exclude. `Default` resolves to
    /// the opposite of the current effective state ([`is_in_context`]), so the
    /// toggle always lands on an explicit `Forced*` value — it never produces
    /// `Default`. Resetting to `Default` is the job of the `r` reset intent.
    ///
    /// [`is_in_context`]: crate::feat::session::chat_entry::ChatEntry::is_in_context
    ///
    /// Returns `Some(entry_id)` if the override was changed, `None` if no-op
    /// (entry was already in the toggled state) or no entry is selected.
    pub fn toggle_entry_ignored(&mut self) -> Option<crate::protocol::ChatEntryId> {
        let hist_idx = self.selected_history_index()?;
        let entry = self.core.history.get(hist_idx)?;
        let pressed_id = entry.id.clone();
        let new_value = match entry.context_override() {
            ContextOverride::ForcedInclude => ContextOverride::ForcedExclude,
            ContextOverride::ForcedExclude => ContextOverride::ForcedInclude,
            ContextOverride::Default => {
                if entry.is_in_context() {
                    ContextOverride::ForcedExclude
                } else {
                    ContextOverride::ForcedInclude
                }
            }
        };
        // Chunk semantics: the toggle applies to the whole tool loop.
        let changed = self
            .edit_history()
            .set_context(&pressed_id, new_value, &ChangeSource::User);
        (!changed.is_empty()).then_some(pressed_id)
    }

    /// Set the context override on the currently selected entry to a specific
    /// value (not a toggle). Used by the x-sweep to apply a captured state.
    ///
    ///
    /// Returns `Some(entry_id)` if the override was changed, `None` if no-op
    /// (entry was already at the target state) or no entry is selected.
    pub fn set_entry_context_override(
        &mut self,
        override_state: ContextOverride,
    ) -> Option<crate::protocol::ChatEntryId> {
        let hist_idx = self.selected_history_index()?;
        let entry = self.core.history.get(hist_idx)?;
        let pressed_id = entry.id.clone();
        // Chunk semantics: the sweep applies to the whole tool loop.
        let changed =
            self.edit_history()
                .set_context(&pressed_id, override_state, &ChangeSource::User);
        (!changed.is_empty()).then_some(pressed_id)
    }

    /// Set the context override on the entry with the given id to a specific
    /// value. Unlike [`Self::set_entry_context_override`] this targets an
    /// entry by id rather than the cursor, so callers can write chunks
    /// without moving the selection.
    ///
    /// Returns `Some(entry_id)` if the override was changed, `None` if no-op
    /// (entry was already at the target state) or the id is unknown.
    pub fn set_entry_context_override_by_id(
        &mut self,
        id: &ChatEntryId,
        override_state: ContextOverride,
    ) -> Option<ChatEntryId> {
        // Chunk semantics: the write applies to the whole tool loop.
        let changed = self
            .edit_history()
            .set_context(id, override_state, &ChangeSource::User);
        (!changed.is_empty()).then(|| id.clone())
    }

    /// After a sweep changes an entry from excluded to in-context, propagate
    /// `shown_ignored_blocks` to any new sub-blocks created by the split.
    ///
    /// When an entry inside a shown (expanded) ignored block becomes in-context,
    /// it splits the block. If the original block was shown, the new forward
    /// sub-block should also be shown so entries remain visible.
    ///
    /// No-op if the entry was not inside a shown block.
    pub fn propagate_shown_on_unignore(&mut self, entry_id: &ChatEntryId) {
        let Some(idx) = self.core.history.iter().position(|e| e.id == *entry_id) else {
            return;
        };

        // Scan backward to find the containing block's start.
        let mut block_start = idx;
        while block_start > 0 {
            let Some(prev) = self.core.history.get(block_start - 1) else {
                break;
            };
            if prev.is_in_context() || prev.pin_position.is_some() {
                break;
            }
            block_start -= 1;
        }

        let Some(block_entry) = self.core.history.get(block_start) else {
            return;
        };
        let block_representative = block_entry.id.clone();
        let was_shown = self.with_view(
            |v| v.shown_ignored_blocks.contains(&block_representative),
            || false,
        );
        if !was_shown {
            return; // Block was not shown — nothing to propagate.
        }

        let forward_start = idx + 1;
        // Scan forward from the changed entry to find a new forward sub-block.
        let Some(forward_entry) = self.core.history.get(forward_start) else {
            return;
        };
        if forward_entry.is_in_context() || forward_entry.pin_position.is_some() {
            return; // No excluded entries after — no sub-block to create.
        }

        // The forward sub-block's representative is its first entry.
        let forward_representative = forward_entry.id.clone();
        self.update_view(|v| {
            v.shown_ignored_blocks.insert(forward_representative);
        });
    }

    /// Rebuild the visual items list from the current history and
    /// `shown_ignored_blocks`. Needed during sweep operations to keep the
    /// visual items consistent with mutated entry state between render passes.
    pub fn rebuild_visual_items(&self) {
        use crate::feat::ui::chat_log::visual_item::{
            DEFAULT_MIN_COLLAPSE_COUNT, PROXIMITY_COUNT, build_visual_items,
        };
        let shown = self.shown_ignored_blocks_snapshot();
        let items = build_visual_items(
            &self.core.history,
            &shown,
            PROXIMITY_COUNT,
            DEFAULT_MIN_COLLAPSE_COUNT,
        );
        self.set_visual_items(items);
    }

    /// Returns the sweep target state if an active sweep exists and has not
    /// expired (>100ms since last press). Consumes (clears) the sweep state
    /// regardless of expiry - the caller must re-store it if continuing.
    pub fn take_ignore_sweep(&mut self) -> Option<ContextOverride> {
        let sweep = self.update_view_taking(|v| v.ignore_sweep.take());
        let (instant, override_state) = sweep?;
        (instant.elapsed() < std::time::Duration::from_millis(100)).then_some(override_state)
    }

    /// Starts or continues a sweep by storing the target state and current time.
    pub fn set_ignore_sweep(&mut self, target: ContextOverride) {
        self.update_view(|v| v.ignore_sweep = Some((std::time::Instant::now(), target)));
    }

    /// Clears the sweep state, resetting to normal toggle behavior.
    pub fn clear_ignore_sweep(&mut self) {
        self.update_view(|v| v.ignore_sweep = None);
    }

    /// Removes and returns the raw sweep state (timestamp + target),
    /// bypassing the expiry check.
    ///
    /// Test seam for sweep expiry: [`Self::take_ignore_sweep`] discards
    /// sweeps older than 100ms, so expiry cannot be observed without a way
    /// to plant and retrieve a stale timestamp.
    #[doc(hidden)]
    pub fn take_ignore_sweep_raw(&mut self) -> Option<(std::time::Instant, ContextOverride)> {
        self.update_view_taking(|v| v.ignore_sweep.take())
    }

    /// Plants the sweep state with an explicit timestamp.
    ///
    /// Test seam companion to [`Self::take_ignore_sweep_raw`]; production
    /// sweeps always stamp `Instant::now` (see [`Self::set_ignore_sweep`]).
    #[doc(hidden)]
    pub fn set_ignore_sweep_at(&mut self, instant: std::time::Instant, target: ContextOverride) {
        self.update_view(|v| v.ignore_sweep = Some((instant, target)));
    }

    /// Whether this session has no history entries.
    ///
    /// A session is "empty" when it has never had any entries pushed -
    /// no user messages, no system messages, nothing.
    /// Not to be confused with [`Self::is_idle`] which checks
    /// streaming/sending/assembling state.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.core.history.is_empty()
    }

    /// Whether this session should be persisted to disk.
    pub fn persist(&self) -> bool {
        self.core.persist
    }

    /// Mark this session as having been meaningfully interacted with by the user.
    /// Once set, the session becomes eligible for persistence.
    pub fn mark_interacted(&mut self) {
        self.core.has_interacted = true;
    }

    /// Whether this session has been interacted with.
    #[must_use]
    pub fn has_interacted(&self) -> bool {
        self.core.has_interacted
    }

    /// Whether this session should be persisted to disk.
    ///
    /// Returns `false` immediately when `persist == false` (explicitly marked
    /// transient — e.g. a one-shot). Otherwise returns `true`
    /// if any of:
    /// - The user has interacted with this session (`has_interacted`)
    /// - The session has a lifecycle (setup/teardown scripts)
    /// - The session was forked from another session
    #[must_use]
    pub fn is_persistable(&self) -> bool {
        if !self.core.persist {
            return false;
        }
        if self.core.lifecycle_name.is_some() {
            return true;
        }
        if self.core.parent_session.is_some() {
            return true;
        }
        if self.core.has_interacted {
            return true;
        }
        false
    }

    /// Append an entry to the history and return its index.
    ///
    /// Implements smart auto-scroll: only resets scroll and advances cursor
    /// to the new entry if the cursor was on the previous last entry (or history
    /// was empty). Otherwise, appends silently - preserving the user's scroll
    /// position and selection.
    ///
    /// Delegates to the history editor ([`HistoryEditor::append`]); all
    /// history writes route through the editor.
    pub fn push_entry(&mut self, entry: ChatEntry) -> usize {
        self.edit_history().append(entry)
    }

    /// Expands `#token` templates and `@/abs/path` image references in a user
    /// entry **without** pushing it onto the history.
    ///
    /// This is the same expansion `push_entry` applies, factored out so callers
    /// that need to inspect the expanded entry (e.g. the multimodal capability
    /// gate before dispatch) can run it ahead of `push_entry` without double
    /// work. Expansion is idempotent: attachable `@path` tokens are rewritten
    /// once on the first pass, while degraded tokens (already recorded in
    /// [`ChatEntryKind::User::outcome`]) stay literal across re-expansion.
    pub fn expand_entry(&self, entry: &mut ChatEntry) {
        let ctx = PathResolveContext::new(&self.core.cwd, &self.core.home);
        expand_user_entry(
            entry,
            &self.core.ephemeral.discovered_prompt_templates,
            &ctx,
        );
    }

    /// Insert an entry at a specific position in the history.
    ///
    /// Used by compaction to place the `Compaction` entry at the boundary
    /// between compacted and non-compacted entries, maintaining logical
    /// vec order.
    ///
    /// Adjusts ephemeral tracking indices (streaming, tool results) that
    /// reference positions >= the insertion point.
    ///
    /// Returns the index where the entry was inserted.
    pub fn insert_entry_at(&mut self, index: usize, entry: ChatEntry) -> usize {
        let clamped = index.min(self.core.history.len());
        self.core.history.insert(clamped, entry);
        // Delegate index shifting to the machine (handles all 4 streaming fields).
        self.core
            .ephemeral
            .machine
            .shift_streaming_indices_for_insert_at(clamped);
        clamped
    }

    /// Lazily create the Assistant entry for the current stream.
    ///
    /// Called on first `append_stream_token`, `finish_streaming`,
    /// `begin_tool_call`, or `cancel_streaming`. No-op if the entry
    /// already exists or the session is not streaming.
    fn ensure_assistant_entry(&mut self, dispatched_at: jiff::Timestamp) {
        if self
            .core
            .ephemeral
            .machine
            .streaming_entry_index()
            .is_some()
            || !matches!(self.core.ephemeral.machine.kind(), PhaseKind::Streaming)
        {
            return;
        }
        let mut entry = ChatEntry::assistant("");
        entry.timing = EntryTiming::streamed(dispatched_at);
        entry.timing.set_first_token();
        let index = self.push_entry(entry);
        self.core.ephemeral.machine.set_streaming_entry_index(index);
    }

    fn finish_streaming_entry(&mut self, idx: usize) {
        self.edit_history()
            .with_entry_at_mut(idx, |entry| entry.timing.finish());
    }

    /// Finalize the streaming thinking entry's timing, if it hasn't already been.
    ///
    /// Guarded to be a no-op if `finished_at` is already set, so safety-net
    /// calls from `finish_streaming`/`cancel_streaming` don't move an
    /// already-recorded timestamp.
    pub fn finish_thinking_entry(&mut self, idx: usize) {
        self.edit_history().with_entry_at_mut(idx, |entry| {
            if entry.timing.finished_at().is_none() {
                entry.timing.finish();
            }
        });
    }

    /// Begin a new streaming response.
    //
    // Phase 1 wiring: delegates to machine.on_first_token() and syncs the
    // legacy phase field. If the machine rejects the transition (e.g. not in
    // Sending), logs a warning and returns without changing state - matching
    // the old soft-guard behavior.
    //
    // Note: The old code accepted both `Sending` and `Idle` phases. The machine
    // only accepts `Sending → Streaming`. To maintain backward compat during the
    // migration, we also accept `Idle → Streaming` by first transitioning to
    // `Sending` then to `Streaming`.
    pub fn begin_streaming(&mut self) {
        use crate::feat::session::phase_machine::PhaseTransitions;
        // If Idle, first transition to Sending (some callers skip begin_sending()).
        if matches!(self.core.ephemeral.machine.kind(), PhaseKind::Idle)
            && let Err(e) = self.core.ephemeral.machine.on_dispatch_message()
        {
            tracing::warn!(
                current_phase = ?self.core.ephemeral.machine.kind(),
                err = %e,
                "begin_streaming: on_dispatch_message rejected - ignoring"
            );
            return;
        }
        if let Err(e) = self.core.ephemeral.machine.on_first_token() {
            tracing::warn!(
                current_phase = ?self.core.ephemeral.machine.kind(),
                err = %e,
                "begin_streaming: machine rejected transition - ignoring"
            );
        }
        self.core.last_history_activity_at = Timestamp::now();
    }

    /// Append a token to the streaming assistant entry.
    ///
    /// Lazily creates the Assistant entry if this is the first token.
    ///
    /// Soft guard: if the session is not streaming, logs a warning and returns an error.
    ///
    /// # Errors
    ///
    /// Returns `Err(StreamingError::NoStreamingEntry)` if the session is not in Streaming phase.
    pub fn append_stream_token<S>(
        &mut self,
        token: S,
        dispatched_at: jiff::Timestamp,
    ) -> Result<(), StreamingError>
    where
        S: AsRef<str>,
    {
        if !matches!(self.core.ephemeral.machine.kind(), PhaseKind::Streaming) {
            tracing::warn!(
                current_phase = ?self.core.ephemeral.machine.kind(),
                "append_stream_token called while not streaming - ignoring"
            );
            return Err(StreamingError::NoStreamingEntry);
        }
        self.ensure_assistant_entry(dispatched_at);
        self.core.last_history_activity_at = Timestamp::now();
        self.core.last_provider_activity_at = Timestamp::now();
        let index = self
            .core
            .ephemeral
            .machine
            .streaming_entry_index()
            .ok_or(StreamingError::NoStreamingEntry)?;
        if let Some(entry) = self.edit_history().with_entry_at_mut(index, |entry| {
            if let ChatEntryKind::Assistant(text) = &mut entry.kind {
                text.push_str(token.as_ref());
                true
            } else {
                false
            }
        }) && entry
        {
            Ok(())
        } else {
            Err(StreamingError::NotAssistantEntry)
        }
    }

    /// Begin accumulating thinking tokens.
    ///
    /// Appends an empty `Thinking` entry to the history. The Assistant entry
    /// is created lazily later (on first `append_stream_token` or `finish_streaming`),
    /// so entries naturally appear in order: thinking before assistant.
    ///
    /// Soft guard: if the session is not streaming or thinking has already begun,
    /// logs a warning and returns without changing state.
    pub fn begin_thinking(&mut self, dispatched_at: jiff::Timestamp) {
        if !matches!(self.core.ephemeral.machine.kind(), PhaseKind::Streaming) {
            tracing::warn!(
                current_phase = ?self.core.ephemeral.machine.kind(),
                "begin_thinking called while not streaming - ignoring"
            );
            return;
        }
        if self
            .core
            .ephemeral
            .machine
            .streaming_thinking_entry_index()
            .is_some()
        {
            tracing::warn!("begin_thinking called while already thinking - ignoring");
            return;
        }
        let mut entry = ChatEntry::thinking("");
        entry.timing = EntryTiming::streamed(dispatched_at);
        entry.timing.set_first_token();
        // Insert thinking BEFORE the assistant entry when the assistant entry
        // already exists. Some providers (OpenRouter) send reasoning tokens
        // AFTER content tokens, so the assistant entry is already in history.
        // The thinking entry should appear before it in the chat log.
        let index = if let Some(assistant_idx) = self.core.ephemeral.machine.streaming_entry_index()
        {
            self.insert_entry_at(assistant_idx, entry)
        } else {
            self.push_entry(entry)
        };
        self.core
            .ephemeral
            .machine
            .set_streaming_thinking_entry_index(index);
    }

    /// Append a thinking token to the streaming Thinking entry.
    ///
    /// # Panics
    ///
    /// Panics if `begin_thinking()` has not been called.
    ///
    /// # Errors
    ///
    /// Returns a [`StreamingError`] if the session is not in a valid streaming state.
    pub fn append_thinking_token<S>(&mut self, token: S) -> Result<(), StreamingError>
    where
        S: AsRef<str>,
    {
        let index = self
            .core
            .ephemeral
            .machine
            .streaming_thinking_entry_index()
            .ok_or(StreamingError::NoThinkingEntry)?;
        self.core.last_history_activity_at = Timestamp::now();
        self.core.last_provider_activity_at = Timestamp::now();
        if let Some(true) = self.edit_history().with_entry_at_mut(index, |entry| {
            if let ChatEntryKind::Thinking(text) = &mut entry.kind {
                text.push_str(token.as_ref());
                true
            } else {
                false
            }
        }) {
            return Ok(());
        }
        // Original behavior: a non-thinking entry at the index is ignored.
        Ok(())
    }

    /// The index of the streaming thinking entry, if thinking is being accumulated.
    pub fn streaming_thinking_entry_index(&self) -> Option<usize> {
        self.core.ephemeral.machine.streaming_thinking_entry_index()
    }

    /// Mark streaming as finished (normal completion).
    //
    // Phase 1 wiring: delegates to machine.on_stream_completed_finished()
    // and syncs legacy phase field.
    pub fn finish_streaming(&mut self, preserve_assistant: bool, dispatched_at: jiff::Timestamp) {
        use crate::feat::session::phase_machine::PhaseTransitions;
        if preserve_assistant {
            self.ensure_assistant_entry(dispatched_at);
        }

        // Set finished_at on the assistant entry.
        if let Some(idx) = self.core.ephemeral.machine.streaming_entry_index() {
            self.finish_streaming_entry(idx);
        }

        // Safety net: finalize any still-pending thinking entry (pure-reasoning
        // streams that never produced a content token). Must run before the
        // StreamingPhase drop clears the index.
        if let Some(idx) = self.core.ephemeral.machine.streaming_thinking_entry_index() {
            self.finish_thinking_entry(idx);
        }
        if let Err(e) = self.core.ephemeral.machine.on_stream_completed_finished() {
            tracing::warn!(
                current_phase = ?self.core.ephemeral.machine.kind(),
                err = %e,
                "finish_streaming: machine rejected transition"
            );
        }
        // Streaming indices cleared automatically by Phase::Streaming drop.
    }

    /// Cancel streaming but keep partial text in history.
    //
    // Phase 1 wiring: delegates to machine.cancel() and syncs legacy phase.
    pub fn cancel_streaming(&mut self, dispatched_at: jiff::Timestamp) {
        self.ensure_assistant_entry(dispatched_at);

        // Set finished_at on the assistant entry.
        if let Some(idx) = self.core.ephemeral.machine.streaming_entry_index() {
            self.finish_streaming_entry(idx);
        }

        // Safety net: finalize any still-pending thinking entry so its duration
        // resolves even if reasoning was interrupted. Must run before cancel()
        // drops the StreamingPhase and clears streaming_thinking_entry_index().
        if let Some(idx) = self.core.ephemeral.machine.streaming_thinking_entry_index() {
            self.finish_thinking_entry(idx);
        }
        if let Err(e) = self.core.ephemeral.machine.cancel() {
            tracing::warn!(
                current_phase = ?self.core.ephemeral.machine.kind(),
                err = %e,
                "cancel_streaming: machine rejected cancel"
            );
        }
        // All streaming indices cleaned up by StreamingPhase drop on cancel()
    }

    /// Cancel streaming and drain steering fragments plus queued messages back
    /// into the input buffer.
    ///
    /// Used when the user interrupts via ESC-confirm during an active stream.
    /// Steering fragments (drained first) and the display text of each queued
    /// `UserMessage` are joined with `"\n\n---\n\n"` and replace whatever was
    /// in the input box. `ToolContinuation` items are silently discarded.
    /// If nothing was drained, the input box is left untouched.
    pub fn cancel_stream_and_drain(&mut self) {
        self.cancel_streaming(jiff::Timestamp::now());
        let drained_text = self.drain_cancel_chunks().join("\n\n---\n\n");
        if !drained_text.is_empty() {
            self.update_input(|input| input.replace_all(drained_text));
        }
    }

    /// Discard partial streaming entries so a stalled stream can be retried.
    ///
    /// Removes in-progress assistant and thinking entries from history and
    /// clears all streaming indices (assistant, thinking, tool-call, tool-result),
    /// while staying in the `Streaming` phase. The retried stream's first token
    /// creates fresh entries. Committed history (completed user/assistant entries)
    /// is untouched.
    pub fn reset_streaming_entries_for_retry(&mut self) -> usize {
        // Remove every history index we own, then clear the machine indices.
        let mut indices = self.collect_streaming_history_indices();
        indices.sort_unstable_by(|a, b| b.cmp(a));
        indices.dedup();
        let removed = self.edit_history().remove_trailing(&indices);
        self.core.ephemeral.machine.clear_streaming_indices();
        removed
    }

    /// Every history entry index currently tracked by the streaming phase.
    fn collect_streaming_history_indices(&self) -> Vec<usize> {
        let mut indices = Vec::new();
        if let Some(i) = self.core.ephemeral.machine.streaming_entry_index() {
            indices.push(i);
        }
        if let Some(i) = self.core.ephemeral.machine.streaming_thinking_entry_index() {
            indices.push(i);
        }
        indices.extend(
            self.core
                .ephemeral
                .machine
                .streaming_tool_call_indices()
                .values()
                .copied(),
        );
        indices.extend(
            self.core
                .ephemeral
                .machine
                .streaming_tool_result_indices()
                .values()
                .copied(),
        );
        indices
    }

    /// Collects the text chunks drained out of the steering buffer and turn
    /// queue, in cancel-recovery order.
    ///
    /// Steering fragments come first, followed by the display text of each
    /// queued `UserMessage`. `ToolContinuation` items are discarded. The
    /// caller applies whatever separator it wants.
    fn drain_cancel_chunks(&mut self) -> Vec<String> {
        let steering = self.steering_buffer_mut().drain_fragments();
        let queue = self.drain_queue();
        steering
            .into_iter()
            .chain(queue.into_iter().filter_map(|item| match item {
                crate::feat::session::queue_item::QueueItem::UserMessage(entry) => {
                    match &entry.kind {
                        ChatEntryKind::User { display, .. } => Some(display.clone()),
                        _ => None,
                    }
                }
                crate::feat::session::queue_item::QueueItem::ToolContinuation => None,
            }))
            .collect()
    }

    /// Returns the current session lifecycle phase.
    pub fn phase(&self) -> PhaseKind {
        self.core.ephemeral.machine.kind()
    }

    /// Create a placeholder `ToolCall` entry and record its history index.
    ///
    /// Called when `ToolUseStarted` arrives - the tool name is known but arguments
    /// are still streaming in.
    pub fn begin_tool_call(
        &mut self,
        index: usize,
        id: &str,
        name: &str,
        dispatched_at: jiff::Timestamp,
    ) {
        self.ensure_assistant_entry(dispatched_at);
        self.core.last_provider_activity_at = Timestamp::now();
        let mut entry = ChatEntry::tool_call(id, name, "");
        entry.timing = EntryTiming::streamed(dispatched_at);
        entry.timing.set_first_token();
        let history_index = self.push_entry(entry);
        let Some(indices) = self
            .core
            .ephemeral
            .machine
            .streaming_tool_call_indices_mut()
        else {
            tracing::warn!(
                current_phase = ?self.core.ephemeral.machine.kind(),
                index,
                "begin_tool_call called while not streaming - ignoring"
            );
            return;
        };
        indices.insert(index, history_index);
    }

    /// Append an incremental delta to a streaming tool call's arguments.
    ///
    /// `partial_json` is appended to the existing arguments string - it is *not*
    /// the accumulated total.
    ///
    /// # Panics
    ///
    /// Returns an error if no tool call entry is tracked for the given stream index.
    ///
    /// # Errors
    ///
    /// Returns a [`StreamingError`] if the streaming state is invalid or the index is out of bounds.
    pub fn append_tool_call_delta(
        &mut self,
        index: usize,
        partial_json: &str,
    ) -> Result<(), StreamingError> {
        let history_index = self
            .core
            .ephemeral
            .machine
            .streaming_tool_call_indices()
            .get(&index)
            .copied()
            .ok_or(StreamingError::NoToolCallIndex { index })?;
        if let Some(()) = self
            .edit_history()
            .with_entry_at_mut(history_index, |entry| {
                if let ChatEntryKind::ToolCall {
                    ref mut arguments, ..
                } = entry.kind
                {
                    arguments.push_str(partial_json);
                }
            })
        {
            self.core.last_provider_activity_at = Timestamp::now();
        }
        Ok(())
    }

    /// Overwrite a tool call entry with the final complete arguments.
    ///
    /// Searches recent history for a `ToolCall` entry matching the given ID.
    /// If not found (shouldn't happen in normal flow), pushes a new entry.
    /// Preserves any existing `child_session` link on the entry — only the
    /// streamed-partial fields are finalized here.
    pub fn finalize_tool_call(&mut self, id: &str, name: &str, arguments: &str) {
        let finalized = self
            .edit_history()
            .with_last_matching_mut(
                |entry| matches!(&entry.kind, ChatEntryKind::ToolCall { .. }),
                |entry| {
                    let child_session = match &entry.kind {
                        ChatEntryKind::ToolCall { child_session, .. } => child_session.clone(),
                        _ => None,
                    };
                    entry.kind = ChatEntryKind::ToolCall {
                        id: id.to_owned(),
                        name: name.to_owned(),
                        arguments: arguments.to_owned(),
                        child_session,
                    };
                    entry.timing.finish();
                },
            )
            .is_some();
        if finalized {
            self.core.last_provider_activity_at = Timestamp::now();
        } else {
            // If not found (shouldn't happen), push a new entry.
            self.push_entry(ChatEntry::tool_call(id, name, arguments));
        }
    }

    /// Create a pending ToolResult entry when a streaming tool starts executing.
    ///
    /// Creates the entry with `ToolResultStatus::Pending` and empty content,
    /// then tracks its history index for later content appends.
    pub fn begin_tool_result(
        &mut self,
        tool_call_id: &str,
        name: &str,
        dispatched_at: jiff::Timestamp,
    ) {
        // Early return if not in Streaming phase — don't push orphaned entries.
        if self
            .core
            .ephemeral
            .machine
            .streaming_tool_result_indices_mut()
            .is_none()
        {
            tracing::warn!(
                current_phase = ?self.core.ephemeral.machine.kind(),
                tool_call_id,
                "begin_tool_result called while not streaming - ignoring"
            );
            return;
        }

        let mut entry = ChatEntry::tool_result(
            tool_call_id,
            name,
            "",
            crate::feat::session::tool_result_status::ToolResultStatus::Pending,
        );
        entry.timing = EntryTiming::streamed(dispatched_at);
        entry.timing.set_first_token();
        let history_index = self.push_entry(entry);

        // Re-acquire the streaming index map after push_entry releases &mut self.
        if let Some(indices) = self
            .core
            .ephemeral
            .machine
            .streaming_tool_result_indices_mut()
        {
            indices.insert(tool_call_id.to_owned(), history_index);
        }
    }

    /// Append incremental output to a pending ToolResult entry.
    ///
    /// # Panics
    ///
    /// Panics if no pending entry exists for the given `tool_call_id`.
    pub fn append_tool_result_output(
        &mut self,
        tool_call_id: &str,
        output: &str,
        kind: jinn_tools_msg::ToolOutputKind,
    ) {
        let Some(&history_index) = self
            .core
            .ephemeral
            .machine
            .streaming_tool_result_indices()
            .get(tool_call_id)
        else {
            return;
        };
        self.core.last_history_activity_at = jiff::Timestamp::now();
        self.edit_history()
            .with_entry_at_mut(history_index, |entry| {
                if let ChatEntryKind::ToolResult {
                    ref mut content,
                    ref mut is_alert,
                    ..
                } = entry.kind
                {
                    content.push_str(output);
                    if kind == jinn_tools_msg::ToolOutputKind::Alert {
                        *is_alert = true;
                    }
                }
            });
    }

    /// Finalize a pending ToolResult entry with the final content and status.
    ///
    /// If a pending entry exists, updates it with the final content and
    /// success/failure status. If no pending entry exists (non-streaming tool),
    /// pushes a new completed entry.
    ///
    /// Accepts optional truncation metadata and full content from the tool
    /// execution result. When truncation is present, stores both the truncated
    /// content and the original untruncated output.
    /// Finalizes an existing pending ToolResult entry (streaming index first,
    /// then a history scan), returning whether one was found.
    ///
    /// In-place finalization: never reorders entries or splits a tool loop.
    fn finalize_existing_tool_result(
        &mut self,
        tool_call_id: &str,
        content: &str,
        status: crate::feat::session::tool_result_status::ToolResultStatus,
        full_content: Option<String>,
        truncation: Option<jinn_core_types::tool_types::TruncationMeta>,
        pin_position: Option<PinPosition>,
    ) -> bool {
        let streaming_index = self
            .core
            .ephemeral
            .machine
            .streaming_tool_result_indices_mut()
            .and_then(|map| map.remove(tool_call_id));

        let apply = |entry: &mut ChatEntry| {
            if let ChatEntryKind::ToolResult {
                content: entry_content,
                status: entry_status,
                full_content: entry_full_content,
                truncation: entry_truncation,
                pin_position: entry_kind_pin,
                is_alert,
                ..
            } = &mut entry.kind
            {
                content.clone_into(entry_content);
                *entry_status = status;
                *entry_full_content = full_content;
                *entry_truncation = truncation;
                *entry_kind_pin = pin_position;
                // The alert styling is pending-state only — a finished result
                // renders with its normal success/failure look.
                *is_alert = false;
                // Entry-level pin mirrors the kind-level pin so assembly,
                // compaction, and UI consumers read a single field.
                entry.pin_position = pin_position;
                entry.timing.finish();
            }
        };

        match streaming_index {
            Some(index) => self.edit_history().with_entry_at_mut(index, apply).is_some(),
            None => self
                .edit_history()
                .with_last_matching_mut(
                    |entry| {
                        matches!(&entry.kind, ChatEntryKind::ToolResult { id, .. } if id == tool_call_id)
                    },
                    apply,
                )
                .is_some(),
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "mirrors begin_tool_result + pin_position; struct-builder would ripple to all tool actors"
    )]
    pub fn finalize_tool_result(
        &mut self,
        tool_call_id: &str,
        name: &str,
        content: &str,
        success: bool,
        full_content: Option<String>,
        truncation: Option<jinn_core_types::tool_types::TruncationMeta>,
        pin_position: Option<PinPosition>,
    ) {
        let status = if success {
            crate::feat::session::tool_result_status::ToolResultStatus::Success
        } else {
            crate::feat::session::tool_result_status::ToolResultStatus::Failure
        };
        let pin_position_result = pin_position;

        let finalized = self.finalize_existing_tool_result(
            tool_call_id,
            content,
            status,
            full_content.clone(),
            truncation.clone(),
            pin_position,
        );

        if !finalized {
            // No existing entry found - push a new one.
            let mut entry = if let Some(meta) = truncation {
                let full = full_content.unwrap_or_default();
                ChatEntry::tool_result_truncated(
                    tool_call_id,
                    name,
                    content.to_owned(),
                    full,
                    status,
                    meta,
                )
            } else {
                ChatEntry::tool_result(tool_call_id, name, content, status)
            };
            // Propagate tool-requested pin onto both the kind variant and
            // the entry-level field so assembly/compaction read a single source.
            if let ChatEntryKind::ToolResult {
                pin_position: ref mut kp,
                ..
            } = entry.kind
            {
                *kp = pin_position;
            }
            entry.pin_position = pin_position;
            self.push_entry(entry);
        }

        // A tool-requested pin (skill/save_plan bodies) expands to the whole
        // tool loop via the editor, so the pinned result can never be
        // separated from its call by pruning or compaction.
        if let Some(position) = pin_position_result {
            let result_id = self
                .core
                .history
                .iter()
                .rev()
                .find(|entry| {
                    matches!(&entry.kind, ChatEntryKind::ToolResult { id, .. } if id == tool_call_id)
                })
                .map(|entry| entry.id.clone());
            if let Some(id) = result_id {
                self.edit_history().pin(&id, position);
            }
        }
    }

    /// Read-only access to the turn dispatch queue items.
    pub fn queue(
        &self,
    ) -> &std::collections::VecDeque<crate::feat::session::queue_item::QueueItem> {
        self.core.ephemeral.message_queue.items()
    }

    /// Number of items waiting in the queue.
    pub fn queue_len(&self) -> usize {
        self.core.ephemeral.message_queue.len()
    }

    /// Push an item onto the back of the queue.
    pub fn enqueue(&mut self, item: crate::feat::session::queue_item::QueueItem) {
        self.core.ephemeral.message_queue.enqueue(item);
    }

    /// Push an item onto the front of the queue (for priority items).
    pub fn enqueue_front(&mut self, item: crate::feat::session::queue_item::QueueItem) {
        self.core.ephemeral.message_queue.enqueue_front(item);
    }

    /// Pop the front item from the queue, if any.
    ///
    /// The turn-dispatch slice's queue actor is the production caller; the
    /// queue lives on the session, so the pop must be reachable there.
    pub fn dequeue(&mut self) -> Option<crate::feat::session::queue_item::QueueItem> {
        self.core.ephemeral.message_queue.pop()
    }

    /// Drain all queued items, returning them in order.
    pub(in crate::feat) fn drain_queue(
        &mut self,
    ) -> std::collections::VecDeque<crate::feat::session::queue_item::QueueItem> {
        self.core.ephemeral.message_queue.drain()
    }

    /// Read-only access to the session profile.
    pub fn profile(&self) -> &SessionProfile {
        &self.core.profile
    }

    /// Mutable access to the session profile.
    pub fn profile_mut(&mut self) -> &mut SessionProfile {
        &mut self.core.profile
    }

    /// Set the model selection for this session.
    ///
    /// When switching to an alloy, any pinned OpenRouter endpoint is cleared:
    /// an endpoint pin is model-specific and incoherent across a rotating set.
    pub fn set_model(&mut self, model: ModelSelection) {
        if matches!(model, ModelSelection::Alloy { .. }) {
            self.core.profile.endpoint = None;
        }
        self.core.profile.model = model;
    }

    /// Whether a tool is enabled for this session.
    ///
    /// Returns `true` if the tool name is not in the disabled set.
    /// An empty disabled set means all tools are enabled.
    #[must_use]
    pub fn is_tool_enabled(&self, tool_name: &str) -> bool {
        !self.core.profile.disabled_tools.contains(tool_name)
    }

    /// Read-only access to this session's disabled tool names.
    ///
    /// Opt-out model: tools not in this set are enabled.
    pub fn disabled_tools(&self) -> &HashSet<String> {
        &self.core.profile.disabled_tools
    }

    /// Replace the disabled tool set for this session.
    ///
    /// Used by the tool picker to commit toggle state.
    pub fn set_disabled_tools(&mut self, tools: HashSet<String>) {
        self.core.profile.disabled_tools = tools;
    }

    /// Read-only access to this session's enabled MCP server names.
    ///
    /// Opt-in model: only servers in this set are active for the session.
    #[must_use]
    pub fn enabled_mcp_servers(&self) -> &std::collections::BTreeSet<String> {
        &self.core.enabled_mcp_servers
    }

    /// Returns `true` if the named MCP server is enabled for this session.
    #[must_use]
    pub fn is_mcp_server_enabled(&self, server: &str) -> bool {
        self.core.enabled_mcp_servers.contains(server)
    }

    /// Enables an MCP server for this session.
    ///
    /// Returns `true` if the server was not previously enabled (i.e. this call
    /// changed state).
    pub fn enable_mcp_server(&mut self, server: &str) -> bool {
        self.core.enabled_mcp_servers.insert(server.to_owned())
    }

    /// Disables an MCP server for this session.
    ///
    /// Returns `true` if the server was previously enabled (i.e. this call
    /// changed state).
    pub fn disable_mcp_server(&mut self, server: &str) -> bool {
        self.core.enabled_mcp_servers.remove(server)
    }

    /// Replaces the entire enabled MCP server set for this session.
    ///
    /// Used by the MCP picker to commit toggle state.
    pub fn set_enabled_mcp_servers(&mut self, servers: std::collections::BTreeSet<String>) {
        self.core.enabled_mcp_servers = servers;
    }

    /// Read-only access to this session's live MCP server connection statuses.
    #[must_use]
    pub fn mcp_server_status(
        &self,
    ) -> &std::collections::BTreeMap<String, jinn_mcp_msg::McpConnectionStatus> {
        &self.core.mcp_server_status
    }

    /// Sets the live connection status for one MCP server in this session.
    ///
    /// Owned by `McpCoordinatorActor`, driven by `McpServerStatus` events.
    pub fn set_mcp_server_status(
        &mut self,
        server: &str,
        status: jinn_mcp_msg::McpConnectionStatus,
    ) {
        self.core
            .mcp_server_status
            .insert(server.to_owned(), status);
    }

    /// Returns the latest captured stderr tail per MCP server for this session.
    ///
    /// Owned by `McpCoordinatorActor`, driven by `McpServerLog` events.
    pub fn mcp_server_stderr(&self) -> &std::collections::BTreeMap<String, String> {
        &self.core.mcp_server_stderr
    }

    /// Sets the captured stderr tail for one MCP server in this session.
    ///
    /// Owned by `McpCoordinatorActor`, driven by `McpServerLog` events.
    pub fn set_mcp_server_stderr(&mut self, server: &str, tail: String) {
        self.core.mcp_server_stderr.insert(server.to_owned(), tail);
    }

    /// Returns `true` if the skill is enabled for this session.
    ///
    /// An empty disabled set means all skills are enabled.
    #[must_use]
    pub fn is_skill_enabled(&self, skill_name: &str) -> bool {
        !self.core.profile.disabled_skills.contains(skill_name)
    }

    /// Read-only access to this session's disabled skill names.
    ///
    /// Opt-out model: skills not in this set are enabled.
    pub fn disabled_skills(&self) -> &HashSet<String> {
        &self.core.profile.disabled_skills
    }

    /// Replace the disabled skill set for this session.
    ///
    /// Used by the skill picker to commit toggle state.
    pub fn set_disabled_skills(&mut self, skills: HashSet<String>) {
        self.core.profile.disabled_skills = skills;
    }
    /// Compute the set of skill names that are currently loaded in this session.
    ///
    /// A skill is considered loaded if its body is present in history as a pinned
    /// ToolResult from the `skill` tool whose content begins with `<skill name="X"`.
    pub fn loaded_skills(&self) -> HashSet<String> {
        use crate::feat::session::chat_entry::ChatEntryKind;
        use crate::feat::skills::parse_loaded_skill_name;

        let mut out = HashSet::new();
        for entry in self.history() {
            if !entry.is_pinned() {
                continue;
            }
            let ChatEntryKind::ToolResult {
                name: tool_name,
                content,
                ..
            } = &entry.kind
            else {
                continue;
            };
            if tool_name != "skill" {
                continue;
            }
            // Skill bodies are pinned as `<skill name="X" ...>` — extract X.
            let Some(skill_name) = parse_loaded_skill_name(content) else {
                continue;
            };
            out.insert(skill_name.to_owned());
        }
        out
    }

    /// The model selection for this session.
    pub fn model_selection(&self) -> &ModelSelection {
        &self.core.profile.model
    }
    pub fn model(&self) -> &ModelSelection {
        &self.core.profile.model
    }

    /// Mark the session as having dispatched a message to the LLM.
    //
    // Phase 1 wiring: delegates to machine.on_dispatch_message() and syncs
    // the legacy phase field.
    pub fn begin_sending(&mut self) {
        use crate::feat::session::phase_machine::PhaseTransitions;
        if let Err(e) = self.core.ephemeral.machine.on_dispatch_message() {
            tracing::warn!(
                current_phase = ?self.core.ephemeral.machine.kind(),
                err = %e,
                "begin_sending: machine rejected transition - ignoring"
            );
        }
        self.core.last_history_activity_at = Timestamp::now();
    }

    /// Clear the sending flag (called when the first stream token arrives).
    //
    /// Complete the sending phase via the machine's validated transition.
    ///
    /// This should be called when a tool batch completes and the tool loop
    /// is disabled. The machine reads the `tool_loop_disabled` flag and
    /// transitions `Sending → Idle` (if set) or `Sending → Streaming` (if not).
    ///
    /// The caller must ensure `set_tool_loop_disabled(true)` has been called
    /// before this method if the tool loop should be terminated.
    pub fn finish_sending_via_machine(&mut self) {
        use crate::feat::session::phase_machine::PhaseTransitions;
        if let Err(e) = self.core.ephemeral.machine.on_tool_batch_completed() {
            tracing::warn!(
                current_phase = ?self.core.ephemeral.machine.kind(),
                err = %e,
                "finish_sending_via_machine: machine rejected transition - ignoring"
            );
        }
    }

    /// Transition to Working phase (a background operation started).
    ///
    /// Increment the busy counter. Called when a background operation starts.
    /// The count is ephemeral (not persisted).
    pub fn begin_busy(&mut self) {
        self.core.ephemeral.busy_count += 1;
    }

    /// Decrement the busy counter (floor at 0). Called when one background
    /// operation completes. Returns the new count.
    pub fn complete_busy(&mut self) -> usize {
        self.core.ephemeral.busy_count = self.core.ephemeral.busy_count.saturating_sub(1);
        self.core.ephemeral.busy_count
    }

    /// Hard-reset the busy counter to zero. Cancels all tracked operations.
    pub fn cancel_busy(&mut self) {
        self.core.ephemeral.busy_count = 0;
    }

    /// Returns the current number of active background operations.
    pub fn busy_count(&self) -> usize {
        self.core.ephemeral.busy_count
    }

    /// Returns `true` when any background operation is in progress.
    pub fn is_busy(&self) -> bool {
        self.core.ephemeral.busy_count > 0
    }

    /// The current scroll offset (lines to skip from top).
    ///
    /// Returns `None` when auto-scrolled to the bottom, or `Some(n)` when
    /// the user has manually scrolled to a specific offset.
    pub fn scroll_offset(&self) -> Option<u16> {
        self.with_view(|v| v.scroll_offset, || None)
    }

    /// Whether the conversation is scrolled to the bottom (auto-scroll position).
    pub fn is_at_bottom(&self) -> bool {
        self.with_view(|v| v.scroll_offset.is_none(), || true)
    }

    /// Scroll up (toward older messages) by the given number of lines.
    ///
    /// If currently at the bottom (auto-scroll), resolves to `last_max_offset` first
    /// so the scroll is relative to the actual bottom position.
    /// Seeds an explicit scroll offset (test seam for "already scrolled"
    /// arrangements; production scrolls always move relative to the
    /// current offset).
    #[doc(hidden)]
    pub fn set_scroll_offset(&mut self, offset: Option<u16>) {
        self.update_view(|v| v.scroll_offset = offset);
    }

    pub fn scroll_up(&mut self, amount: u16) {
        self.update_view(|v| {
            let current = v
                .scroll_offset
                .unwrap_or(v.last_max_offset.load(Ordering::Relaxed));
            v.scroll_offset = Some(current.saturating_sub(amount));
        });
    }

    /// Scroll down (toward newer messages) by the given number of lines.
    ///
    /// If the resulting offset reaches or exceeds `last_max_offset`, resets to
    /// auto-scroll (bottom).
    pub fn scroll_down(&mut self, amount: u16) {
        self.update_view(|v| {
            let current = v
                .scroll_offset
                .unwrap_or(v.last_max_offset.load(Ordering::Relaxed));
            let next = current.saturating_add(amount);
            if next >= v.last_max_offset.load(Ordering::Relaxed) {
                v.scroll_offset = None;
            } else {
                v.scroll_offset = Some(next);
            }
        });
    }

    /// Reset scroll to show the bottom of the conversation.
    pub fn reset_scroll(&mut self) {
        self.update_view(|v| v.scroll_offset = None);
    }

    /// Scroll to the very top of the conversation.
    pub fn scroll_to_top(&mut self) {
        self.update_view(|v| v.scroll_offset = Some(0));
    }

    /// Scroll to the very bottom of the conversation (auto-scroll).
    pub fn scroll_to_bottom(&mut self) {
        self.update_view(|v| v.scroll_offset = None);
    }

    /// Scroll the chat log so that the currently selected entry is visible.
    ///
    /// Uses `entry_line_ranges` and `viewport_height` (set by the renderer
    /// each frame) to compute the scroll offset that brings the selected
    /// entry into view. This is essentially the same logic as the renderer's
    /// scroll-to-selected adjustment, but applied as a state mutation for
    /// intent handlers.
    ///
    /// No-op if no entry is selected or if line range data is unavailable.
    pub fn scroll_to_selected(&mut self) {
        let Some(selected_idx) = self.selected_entry_index() else {
            return;
        };

        // Read phase: gather the render caches, then decide.
        let decision = self.with_view(
            |v| {
                let ranges = v.entry_line_ranges.read();
                let &(start, end) = ranges.get(selected_idx)?;
                let viewport_height = v.viewport_height.load(Ordering::Relaxed);
                if viewport_height == 0 {
                    return None;
                }
                let blank_count = v.blank_count.load(Ordering::Relaxed);
                let current_offset = v.rendered_scroll_offset.load(Ordering::Relaxed);

                let abs_start = start.saturating_add(blank_count);
                let abs_end = end.saturating_add(blank_count);
                let entry_height = abs_end.saturating_sub(abs_start);

                if entry_height <= viewport_height {
                    // Entry fits in viewport - adjust only if it's outside.
                    if abs_start < current_offset {
                        Some(abs_start)
                    } else if abs_end > current_offset.saturating_add(viewport_height) {
                        Some(abs_end.saturating_sub(viewport_height))
                    } else {
                        // Already visible - no change needed.
                        None
                    }
                } else {
                    // Entry is taller than viewport - align top.
                    if abs_start >= current_offset.saturating_add(viewport_height) {
                        Some(abs_start)
                    } else if abs_end <= current_offset {
                        Some(abs_end.saturating_sub(viewport_height))
                    } else {
                        // Already overlapping - no change needed.
                        None
                    }
                }
            },
            || None,
        );

        let Some(new_offset) = decision else {
            return;
        };

        // Write phase: clamp and apply as the new scroll intent.
        self.update_view(|v| {
            let clamped = new_offset.min(v.last_max_offset.load(Ordering::Relaxed));
            if clamped >= v.last_max_offset.load(Ordering::Relaxed) {
                v.scroll_offset = None;
            } else {
                v.scroll_offset = Some(clamped);
            }
        });
    }

    /// Update the cached maximum scroll offset from the renderer.
    ///
    /// Called by the chat log element during each render so that
    /// scroll handlers can resolve the "at bottom" state into a concrete offset.
    pub fn set_last_max_offset(&self, max_offset: u16) {
        self.update_view(|v| v.last_max_offset.store(max_offset, Ordering::Relaxed));
    }

    /// Returns the screen-space Y coordinate of the top of the currently-selected
    /// chat entry within the chat-log area, or `None` if no entry is selected or
    /// the render-pipeline cache is empty.
    ///
    /// The returned Y is in terminal (absolute) coordinates: it already incorporates
    /// `chat_log_area_y` and `blank_count`. Callers can pass it directly as the
    /// `entry_top_y` argument to
    /// [`audit_popup_rect`](crate::feat::ui::chat_log::audit_popup::audit_popup_rect).
    ///
    /// If the selected entry's top is scrolled above the viewport, returns
    /// `chat_log_area_y` (clamped to the top of the chat-log area).
    ///
    /// Returns a meaningful value only after the chat-log render pipeline has
    /// populated the cached fields for the current frame.
    pub fn selected_entry_screen_y(&self, chat_log_area_y: u16) -> Option<u16> {
        let vi_idx = self.selected_entry_index()?;
        self.with_view(
            |v| {
                let ranges = v.entry_line_ranges.read();
                let &(start, _end) = ranges.get(vi_idx)?;

                let blank_count = v.blank_count.load(Ordering::Relaxed);
                let scroll_offset = v.rendered_scroll_offset.load(Ordering::Relaxed);

                // wrapped-line coord of entry top, with bottom-alignment blank padding
                let abs_start = start.saturating_add(blank_count);

                // viewport top in the same coord space
                let viewport_top = scroll_offset;

                // visible-Y offset within viewport (0 = top of chat-log area)
                let viewport_offset = abs_start.saturating_sub(viewport_top);

                // absolute screen Y; clamped to chat-log area top
                Some(chat_log_area_y.saturating_add(viewport_offset))
            },
            || None,
        )
    }

    /// Store the rendered scroll offset (actual viewport position after clamping
    /// and scroll-to-selected adjustment). Called by the render pipeline each frame.
    pub fn set_rendered_scroll_offset(&self, offset: u16) {
        self.update_view(|v| v.rendered_scroll_offset.store(offset, Ordering::Relaxed));
    }

    /// Store per-entry wrapped line ranges computed by the renderer.
    ///
    /// `entry_line_ranges[i] = (start_wrapped_line, end_wrapped_line)` in the
    /// wrapped coordinate space. Called each frame by the chat log renderer.
    pub fn set_entry_line_ranges(&self, ranges: Vec<(u16, u16)>) {
        self.update_view(|v| *v.entry_line_ranges.write() = ranges);
    }

    /// Store the viewport height (render area height) from the renderer.
    pub fn set_viewport_height(&self, height: u16) {
        self.update_view(|v| v.viewport_height.store(height, Ordering::Relaxed));
    }

    /// Read the cached viewport height.
    pub fn viewport_height_value(&self) -> u16 {
        self.with_view(|v| v.viewport_height.load(Ordering::Relaxed), || 0)
    }

    /// Store the blank line count prepended for bottom-alignment.
    pub fn set_blank_count(&self, count: u16) {
        self.update_view(|v| v.blank_count.store(count, Ordering::Relaxed));
    }

    /// Returns the range of entry indices visible in the current viewport.
    ///
    /// Uses `entry_line_ranges`, `blank_count`, `scroll_offset`, and
    /// `viewport_height` to determine which entries have at least one line
    /// visible. Returns an empty range if no entries are visible or viewport
    /// data is unavailable.
    pub fn visible_entry_range(&self) -> Range<usize> {
        self.with_view(
            |v| {
                let ranges = v.entry_line_ranges.read().clone();
                if ranges.is_empty() {
                    return 0..0;
                }

                let viewport_height = v.viewport_height.load(Ordering::Relaxed);
                let blank_count = v.blank_count.load(Ordering::Relaxed);
                let scroll_offset = v.rendered_scroll_offset.load(Ordering::Relaxed);

                let viewport_top = scroll_offset;
                let viewport_bottom = scroll_offset.saturating_add(viewport_height);

                let mut first_visible = None;
                let mut last_visible = None;

                for (i, &(start, end)) in ranges.iter().enumerate() {
                    let abs_start = start.saturating_add(blank_count);
                    let abs_end = end.saturating_add(blank_count);
                    if abs_end > viewport_top && abs_start < viewport_bottom {
                        if first_visible.is_none() {
                            first_visible = Some(i);
                        }
                        last_visible = Some(i);
                    }
                }

                match (first_visible, last_visible) {
                    (Some(first), Some(last)) => first..last + 1,
                    _ => 0..0,
                }
            },
            || 0..0,
        )
    }

    /// Move the cursor to the first entry visible in the viewport.
    ///
    /// Resolves through visual items. Collapsed blocks are selectable.
    /// No-op if no entries are visible.
    pub fn move_cursor_to_first_visible(&mut self) {
        let range = self.visible_entry_range();
        if range.is_empty() {
            return;
        }
        let items = self.visual_items_snapshot();
        if items.is_empty() {
            // Fallback: use raw history index when visual items not yet computed.
            self.set_selected_entry_index(range.start);
        } else {
            let mut idx = range.start;
            while idx < range.end {
                let selectable = match items.get(idx) {
                    Some(VisualItem::CollapsedIgnoredBlock { .. }) => true,
                    Some(VisualItem::Entry(hist_idx)) => self
                        .core
                        .history
                        .get(*hist_idx)
                        .is_some_and(|e| !e.is_empty_assistant()),
                    None => false,
                };
                if selectable {
                    break;
                }
                idx += 1;
            }
            if idx < items.len() {
                let selectable = match items.get(idx) {
                    Some(VisualItem::CollapsedIgnoredBlock { .. }) => true,
                    Some(VisualItem::Entry(hist_idx)) => self
                        .core
                        .history
                        .get(*hist_idx)
                        .is_some_and(|e| !e.is_empty_assistant()),
                    None => false,
                };
                if selectable {
                    self.set_selected_entry_index(idx);
                }
            }
        }
    }

    /// Move the cursor to the last entry visible in the viewport.
    ///
    /// Resolves through visual items. Collapsed blocks are selectable.
    /// No-op if no entries are visible.
    pub fn move_cursor_to_last_visible(&mut self) {
        let range = self.visible_entry_range();
        if range.is_empty() {
            return;
        }
        let items = self.visual_items_snapshot();
        if items.is_empty() {
            // Fallback: use raw history index when visual items not yet computed.
            self.set_selected_entry_index(range.end.saturating_sub(1));
        } else {
            let mut idx = range.end.saturating_sub(1);
            while idx > range.start {
                let selectable = match items.get(idx) {
                    Some(VisualItem::CollapsedIgnoredBlock { .. }) => true,
                    Some(VisualItem::Entry(hist_idx)) => self
                        .core
                        .history
                        .get(*hist_idx)
                        .is_some_and(|e| !e.is_empty_assistant()),
                    None => false,
                };
                if selectable {
                    break;
                }
                idx = idx.saturating_sub(1);
            }
            let selectable = match items.get(idx) {
                Some(VisualItem::CollapsedIgnoredBlock { .. }) => true,
                Some(VisualItem::Entry(hist_idx)) => self
                    .core
                    .history
                    .get(*hist_idx)
                    .is_some_and(|e| !e.is_empty_assistant()),
                None => false,
            };
            if selectable {
                self.set_selected_entry_index(idx);
            }
        }
    }

    /// Restore conversation history from a persisted snapshot.
    ///
    /// Replaces the current history with the given entries. Used by session
    /// persistence to rehydrate a session from disk.
    pub fn restore_history(&mut self, entries: Vec<ChatEntry>) {
        self.core.history.replace_all(entries);
        let new_cursor = self.core.history.last().map(|e| e.id.clone());
        self.update_view(|v| v.selected_cursor_id = new_cursor);
        self.reset_scroll();
    }

    /// Pin an entry by ID, setting its pin position.
    ///
    /// If no entry with the given ID exists, this is a no-op.
    /// Pin an entry by ID, setting its pin position.
    ///
    /// If no entry with the given ID exists, this is a no-op.
    ///
    /// When pinning an ignored entry inside a shown (expanded) ignored block,
    /// the pinned entry becomes a block splitter in `build_visual_items`.
    /// This propagates `shown_ignored_blocks` to any new forward sub-block
    /// created by the split, keeping all entries visible.
    pub fn pin_entry(&mut self, id: &ChatEntryId, position: PinPosition) {
        let Some(entry) = self.core.history.iter().find(|e| e.id == *id) else {
            return;
        };
        // Captured before pinning: a pin makes the entry in-context, but the
        // propagation below only applies when the entry was ignored.
        let was_ignored = !entry.is_in_context();
        let index = self
            .core
            .history
            .iter()
            .position(|e| e.id == *id)
            .unwrap_or_default();
        // Chunk semantics: pinning a member pins the whole tool loop.
        self.edit_history().pin(id, position);
        self.propagate_shown_after_pin(index, was_ignored);
    }

    /// Propagate `shown_ignored_blocks` when pinning an ignored entry inside a
    /// shown (expanded) block split it.
    fn propagate_shown_after_pin(&mut self, idx: usize, was_ignored: bool) {
        // Propagation: only for entries that were ignored before the pin.
        if !was_ignored {
            return;
        }

        // Scan backward to find the containing block's start.
        // Same boundary rules as `build_visual_items` and `toggle_ignored_block_visibility`.
        let mut block_start = idx;
        while block_start > 0
            && self
                .core
                .history
                .get(block_start - 1)
                .is_some_and(|e| !e.is_in_context())
            && self
                .core
                .history
                .get(block_start - 1)
                .is_some_and(|e| e.pin_position.is_none())
        {
            block_start -= 1;
        }

        let Some(block_entry) = self.core.history.get(block_start) else {
            return;
        };
        let block_representative = block_entry.id.clone();
        let was_shown = self.with_view(
            |v| v.shown_ignored_blocks.contains(&block_representative),
            || false,
        );
        if !was_shown {
            return; // Block was collapsed - nothing to propagate.
        }

        // Scan forward from the pinned entry to find the new forward sub-block.
        let forward_start = idx + 1;
        if forward_start >= self.core.history.len() {
            return; // No entries after the pin.
        }

        let Some(forward_entry) = self.core.history.get(forward_start) else {
            return;
        };
        if forward_entry.is_in_context() || forward_entry.pin_position.is_some() {
            return; // Forward entry is not part of an ignored block.
        }

        // The forward sub-block's representative is its first entry.
        let forward_representative = forward_entry.id.clone();
        self.update_view(|v| {
            v.shown_ignored_blocks.insert(forward_representative);
        });
    }

    /// Unpin an entry by ID, clearing its pin position.
    ///
    /// Chunk semantics: unpinning a member unpins the whole tool loop.
    /// If no entry with the given ID exists, this is a no-op.
    pub fn unpin_entry(&mut self, id: &ChatEntryId) {
        self.edit_history().unpin(id);
    }

    /// Returns all pinned entries in history order.
    pub fn pinned_entries(&self) -> Vec<&ChatEntry> {
        self.core.history.iter().filter(|e| e.is_pinned()).collect()
    }

    /// Select the next entry (moving toward newer messages).
    ///
    /// If nothing is selected, selects the first visual item.
    /// Walks the visual items list, not the raw history.
    /// Collapsed blocks are selectable (so user can press `h` to expand).
    /// Skips empty assistant entries.
    /// Clamps to the last visual-item index.
    /// No-op if visual items list is empty.
    pub fn select_next_entry(&mut self) {
        let items = self.visual_items_snapshot();
        if items.is_empty() {
            // Before first render, fall back to direct history walking.
            self.select_next_entry_fallback();
            return;
        }

        let max = items.len() - 1;
        let start = self
            .selected_entry_index()
            .map_or(0, |i| i.saturating_add(1).min(max));
        let mut idx = start;
        while idx < max {
            let selectable = match items.get(idx) {
                Some(VisualItem::CollapsedIgnoredBlock { .. }) => true,
                Some(VisualItem::Entry(hist_idx)) => self
                    .core
                    .history
                    .get(*hist_idx)
                    .is_some_and(|e: &ChatEntry| !e.is_empty_assistant()),
                None => false,
            };
            if selectable {
                break;
            }
            idx = idx.saturating_add(1);
        }
        let selectable = match items.get(idx) {
            Some(VisualItem::CollapsedIgnoredBlock { .. }) => true,
            Some(VisualItem::Entry(hist_idx)) => self
                .core
                .history
                .get(*hist_idx)
                .is_some_and(|e: &ChatEntry| !e.is_empty_assistant()),
            None => false,
        };
        if selectable {
            self.set_selected_entry_index(idx);
        }
    }

    /// Select the previous entry (moving toward older messages).
    ///
    /// If nothing is selected, selects the last visual item.
    /// Walks the visual items list, not the raw history.
    /// Collapsed blocks are selectable (so user can press `h` to expand).
    /// Skips empty assistant entries.
    /// Clamps to 0.
    /// No-op if visual items list is empty.
    pub fn select_prev_entry(&mut self) {
        let items = self.visual_items_snapshot();
        if items.is_empty() {
            // Before first render, fall back to direct history walking.
            self.select_prev_entry_fallback();
            return;
        }

        let start = self
            .selected_entry_index()
            .map_or(items.len().saturating_sub(1), |i| i.saturating_sub(1));
        let mut idx = start;
        while idx > 0 {
            let selectable = match items.get(idx) {
                Some(VisualItem::CollapsedIgnoredBlock { .. }) => true,
                Some(VisualItem::Entry(hist_idx)) => self
                    .core
                    .history
                    .get(*hist_idx)
                    .is_some_and(|e: &ChatEntry| !e.is_empty_assistant()),
                None => false,
            };
            if selectable {
                break;
            }
            idx = idx.saturating_sub(1);
        }
        let selectable = match items.get(idx) {
            Some(VisualItem::CollapsedIgnoredBlock { .. }) => true,
            Some(VisualItem::Entry(hist_idx)) => self
                .core
                .history
                .get(*hist_idx)
                .is_some_and(|e: &ChatEntry| !e.is_empty_assistant()),
            None => false,
        };
        if selectable {
            self.set_selected_entry_index(idx);
        }
    }

    /// Clear the entry selection.
    pub fn clear_selection(&mut self) {
        self.update_view(|v| v.selected_cursor_id = None);
    }

    /// Set the selected entry index directly.
    ///
    /// Use for programmatic selection (e.g., sidebar pin sync).
    /// Does not validate bounds - caller must ensure index is valid.
    pub fn set_selected_entry_index(&mut self, index: usize) {
        let id = {
            let items = self.visual_items_snapshot();
            if items.is_empty() {
                self.core.history.get(index).map(|e| e.id.clone())
            } else {
                items.get(index).and_then(|item| {
                    crate::feat::ui::chat_log::visual_item::entry_id_from_visual_item(
                        item,
                        &self.core.history,
                    )
                })
            }
        };
        if let Some(id) = id {
            self.update_view(|v| v.selected_cursor_id = Some(id));
        }
    }

    /// Fallback: select next entry by walking raw history.
    /// Used when visual items haven't been computed yet (before first render).
    fn select_next_entry_fallback(&mut self) {
        if self.core.history.is_empty() {
            return;
        }
        let max = self.core.history.len() - 1;
        let start = self
            .selected_entry_index()
            .map_or(0, |i| i.saturating_add(1).min(max));
        let mut idx = start;
        while idx < max
            && self
                .core
                .history
                .get(idx)
                .is_none_or(super::chat_entry::ChatEntry::is_empty_assistant)
        {
            idx = idx.saturating_add(1);
        }
        if self
            .core
            .history
            .get(idx)
            .is_some_and(|e| !e.is_empty_assistant())
        {
            self.set_selected_entry_index(idx);
        }
    }

    /// Fallback: select prev entry by walking raw history.
    /// Used when visual items haven't been computed yet (before first render).
    fn select_prev_entry_fallback(&mut self) {
        if self.core.history.is_empty() {
            return;
        }
        let start = self
            .selected_entry_index()
            .map_or(self.core.history.len().saturating_sub(1), |i| {
                i.saturating_sub(1)
            });
        let mut idx = start;
        while idx > 0
            && self
                .core
                .history
                .get(idx)
                .is_none_or(super::chat_entry::ChatEntry::is_empty_assistant)
        {
            idx = idx.saturating_sub(1);
        }
        if self
            .core
            .history
            .get(idx)
            .is_some_and(|e| !e.is_empty_assistant())
        {
            self.set_selected_entry_index(idx);
        }
    }

    /// Saves the current chat log scroll position as the "pre-pin" snapshot.
    ///
    /// Call this before `sync_chat_log_cursor` changes the viewport.
    /// No-op if a position is already saved (prevents overwriting during
    /// a single Pins visit).
    pub fn save_history_position(&mut self) {
        self.update_view(|v| {
            if v.saved_history_position.is_some() {
                return;
            }
            v.saved_history_position = Some(SavedHistoryPosition {
                scroll_offset: v.scroll_offset,
                selected_cursor_id: v.selected_cursor_id.clone(),
            });
        });
    }

    /// Restores the chat log to the saved "pre-pin" position, if one exists.
    ///
    /// Consumes the saved position (take semantics).
    pub fn restore_history_position(&mut self) {
        self.update_view(|v| {
            if let Some(saved) = v.saved_history_position.take() {
                v.scroll_offset = saved.scroll_offset;
                v.selected_cursor_id = saved.selected_cursor_id;
            }
        });
    }

    /// Discards the saved position without restoring.
    ///
    /// Used when leaving the sidebar to Normal scope - the pin's position
    /// should persist in the chat log.
    pub fn discard_saved_history_position(&mut self) {
        self.update_view(|v| v.saved_history_position = None);
    }

    /// Returns whether there is a saved history position.
    pub fn has_saved_history_position(&self) -> bool {
        self.with_view(|v| v.saved_history_position.is_some(), || false)
    }

    /// The saved pre-pin position, if any (test seam for restore
    /// assertions; production observes it through
    /// [`Self::restore_history_position`]).
    #[doc(hidden)]
    pub fn saved_history_position(&self) -> Option<SavedHistoryPosition> {
        self.with_view(|v| v.saved_history_position.clone(), || None)
    }

    /// The index of the currently selected entry, if any.
    pub fn selected_entry_index(&self) -> Option<usize> {
        let cursor_id = self.selected_cursor_id_owned()?;
        let items = self.visual_items_snapshot();
        if items.is_empty() {
            return self.core.history.iter().position(|e| e.id == cursor_id);
        }
        crate::feat::ui::chat_log::visual_item::resolve_entry_id_to_vi_index(
            &cursor_id,
            &items,
            &self.core.history,
        )
    }

    /// The stored cursor ID (source of truth for selection).
    ///
    /// Unlike `selected_entry_id()` which returns `None` for collapsed blocks,
    /// this always returns the stored ID even when a collapsed block is selected.
    pub fn selected_cursor_id(&self) -> Option<ChatEntryId> {
        self.selected_cursor_id_owned()
    }

    /// The stored cursor ID, cloned. Internal spelling of
    /// [`Self::selected_cursor_id`]; keeps call sites allocation-free when
    /// the copy can be avoided.
    fn selected_cursor_id_owned(&self) -> Option<ChatEntryId> {
        self.with_view(|v| v.selected_cursor_id.clone(), || None)
    }

    /// Set the selected cursor to a specific entry by ID.
    ///
    /// Sets [`SessionUi::selected_cursor_id`] directly, bypassing
    /// visual-item index resolution. Use when the entry ID is already
    /// known (e.g., sidebar pin sync).
    pub fn set_selected_cursor_id(&mut self, id: ChatEntryId) {
        self.update_view(|v| v.selected_cursor_id = Some(id));
    }

    /// The currently selected entry, if any.
    ///
    /// Resolves through the visual items list: returns `None` if the
    /// selected item is a collapsed block rather than a real entry.
    /// Falls back to direct history indexing when visual items are empty
    /// (before the first render).
    pub fn selected_entry(&self) -> Option<&ChatEntry> {
        let vi_idx = self.selected_entry_index()?;
        let items = self.visual_items_snapshot();
        if items.is_empty() {
            // Before first render, visual items haven't been computed yet.
            // Fall back to direct history indexing.
            return self.core.history.get(vi_idx);
        }
        match items.get(vi_idx)? {
            VisualItem::Entry(hist_idx) => self.core.history.get(*hist_idx),
            VisualItem::CollapsedIgnoredBlock { .. } => None,
        }
    }

    /// The ID of the currently selected entry, if any.
    pub fn selected_entry_id(&self) -> Option<&ChatEntryId> {
        self.selected_entry().map(|e| &e.id)
    }

    /// Toggles the expanded state of a tool result entry.
    ///
    /// If the entry is currently expanded, it collapses. Otherwise, it expands.
    pub fn toggle_expand_entry(&mut self, id: ChatEntryId) {
        self.update_view(|v| {
            if v.expanded_entries.contains(&id) {
                v.expanded_entries.remove(&id);
            } else {
                v.expanded_entries.insert(id);
            }
        });
    }

    /// Whether a tool result entry is currently expanded to show full content.
    pub fn is_entry_expanded(&self, id: &ChatEntryId) -> bool {
        self.with_view(|v| v.expanded_entries.contains(id), || false)
    }

    /// Shows the ignored block whose representative is `block_representative`
    /// (test seam: bypasses block-boundary resolution, which production
    /// always goes through `toggle_ignored_block_visibility` for).
    #[doc(hidden)]
    pub fn show_ignored_block(&mut self, block_representative: ChatEntryId) {
        self.update_view(|v| {
            v.shown_ignored_blocks.insert(block_representative);
        });
    }

    /// Toggle visibility of the ignored block containing the given entry.
    ///
    /// Finds the contiguous run of ignored entries containing the entry
    /// identified by `entry_id`, takes the first entry's ID as the block
    /// representative, and toggles it in `shown_ignored_blocks`.
    ///
    /// No-op if the entry is not found or is not ignored.
    pub fn toggle_ignored_block_visibility(&mut self, entry_id: &ChatEntryId) {
        let Some(idx) = self.core.history.iter().position(|e| e.id == *entry_id) else {
            return;
        };
        let Some(entry) = self.core.history.get(idx) else {
            return;
        };
        if entry.is_in_context() {
            return;
        }
        // Scan backward to find the start of the contiguous ignored block.
        // Must match `build_visual_items` block definition: pinned entries
        // act as block splitters even when ignored.
        let mut block_start = idx;
        while block_start > 0
            && self
                .core
                .history
                .get(block_start - 1)
                .is_some_and(|e| !e.is_in_context())
            && self
                .core
                .history
                .get(block_start - 1)
                .is_some_and(|e| e.pin_position.is_none())
        {
            block_start -= 1;
        }
        let Some(block_rep) = self.core.history.get(block_start) else {
            return;
        };
        let block_representative = block_rep.id.clone();
        self.update_view(|v| {
            if v.shown_ignored_blocks.contains(&block_representative) {
                v.shown_ignored_blocks.remove(&block_representative);
            } else {
                v.shown_ignored_blocks.insert(block_representative);
            }
        });
    }

    /// Store the visual items list computed during render.
    pub fn set_visual_items(&self, items: Vec<VisualItem>) {
        self.update_view(|v| *v.visual_items.write() = items);
    }

    /// A snapshot copy of the visual items list computed by the last render.
    ///
    /// Returns an empty vec before the first render. Copying keeps the view
    /// lock out of callers' hands — navigation resolves indices against the
    /// snapshot while the renderer may publish a newer list underneath.
    #[must_use]
    pub fn visual_items_snapshot(&self) -> Vec<VisualItem> {
        self.with_view(|v| v.visual_items.read().clone(), Vec::new)
    }

    /// The visual item at the currently selected position, if any.
    pub fn selected_visual_item(&self) -> Option<VisualItem> {
        let idx = self.selected_entry_index()?;
        self.visual_items_snapshot().get(idx).cloned()
    }

    /// Whether the cursor is currently on a collapsed ignored block.
    pub fn is_selected_collapsed_block(&self) -> bool {
        self.selected_visual_item()
            .is_some_and(|item| matches!(item, VisualItem::CollapsedIgnoredBlock { .. }))
    }

    /// Resolve the selected visual-item index to a history index.
    ///
    /// Returns `None` if nothing is selected or the selected item is a
    /// collapsed block (not a real entry).
    pub fn selected_history_index(&self) -> Option<usize> {
        let vi_idx = self.selected_entry_index()?;
        let items = self.visual_items_snapshot();

        if items.is_empty() {
            // Before first render, visual items haven't been computed yet.
            // Fall back: selected_entry_index IS a history index in this case.
            return Some(vi_idx);
        }
        match items.get(vi_idx)? {
            crate::feat::ui::chat_log::visual_item::VisualItem::Entry(hist_idx) => Some(*hist_idx),
            crate::feat::ui::chat_log::visual_item::VisualItem::CollapsedIgnoredBlock {
                ..
            } => None,
        }
    }

    /// Returns `true` if the given entry is a `ToolCall` that is still
    /// actively streaming arguments from the LLM.
    pub fn is_tool_call_streaming(&self, entry_id: &ChatEntryId) -> bool {
        let Some(idx) = self.core.history.iter().position(|e| e.id == *entry_id) else {
            return false;
        };
        self.core
            .ephemeral
            .machine
            .is_tool_call_at_history_index(idx)
    }
    /// Returns this session's working directory for tool execution.
    pub fn cwd(&self) -> &std::path::Path {
        &self.core.cwd
    }

    /// When this session last saw provider activity (model responses).
    pub fn last_provider_activity_at(&self) -> &Timestamp {
        &self.core.last_provider_activity_at
    }

    /// When this session last saw history activity (new entries appended).
    pub fn last_history_activity_at(&self) -> &Timestamp {
        &self.core.last_history_activity_at
    }

    /// Sets when this session last saw provider activity (streaming/turn).
    pub fn set_last_provider_activity_at(&mut self, ts: Timestamp) {
        self.core.last_provider_activity_at = ts;
    }

    /// Sets when this session last saw history activity (new entries appended).
    pub fn set_last_history_activity_at(&mut self, ts: Timestamp) {
        self.core.last_history_activity_at = ts;
    }

    /// Sets this session's working directory.
    pub fn set_cwd(&mut self, cwd: std::path::PathBuf) {
        self.core.cwd = cwd;
    }

    /// Returns the project directory this session is associated with, if any.
    pub fn project(&self) -> Option<&std::path::Path> {
        self.core.project.as_deref()
    }

    /// Stamps the session's project association. Callers are the projects UI
    /// flow (at session creation) and subagent spawning (inheriting the
    /// parent's stamp); the stamp never follows later cwd changes.
    pub fn set_project(&mut self, project: Option<std::path::PathBuf>) {
        self.core.project = project;
    }

    /// Sets this session's home directory for resolving `@~/path` references.
    pub fn set_home(&mut self, home: std::path::PathBuf) {
        self.core.home = home;
    }

    /// Read-only access to the token ledger.
    pub fn token_ledger(&self) -> &[TokenRecord] {
        &self.core.token_ledger
    }

    /// Push a token record onto the ledger.
    ///
    /// Records are immutable once pushed - this is the only way to add them.
    pub fn push_token_record(&mut self, record: TokenRecord) {
        self.core.token_ledger.push(record);
    }

    /// Read-only access to this session's task list.
    pub fn task_list(&self) -> &jinn_tools_msg::TaskList {
        &self.core.task_list
    }

    /// Mutable access to this session's task list.
    pub fn task_list_mut(&mut self) -> &mut jinn_tools_msg::TaskList {
        &mut self.core.task_list
    }

    /// Update the last token record's received count and cost.
    ///
    /// Called when `StreamCompleted` arrives to finalize the pending record.
    ///
    /// # Errors
    ///
    /// Returns a [`StreamingError`] if the ledger is empty.
    pub fn finalize_last_token_record(
        &mut self,
        tokens_received: u32,
        cost: Option<f64>,
        model_used: Option<String>,
        prompt_tokens: Option<u32>,
        cached_tokens: Option<u32>,
    ) -> Result<(), StreamingError> {
        let last = self
            .core
            .token_ledger
            .last_mut()
            .ok_or(StreamingError::EmptyLedger)?;
        last.tokens_received = tokens_received;
        last.cost = cost;
        last.model_used = model_used;
        last.prompt_tokens = prompt_tokens;
        last.cached_tokens = cached_tokens;
        Ok(())
    }

    /// Sets `model_used` on the last token record (the placeholder pushed at enqueue time).
    /// This makes the model visible in the status bar immediately, before streaming completes.
    pub fn set_last_token_model(&mut self, model: String) {
        if let Some(last) = self.core.token_ledger.last_mut() {
            last.model_used = Some(model);
        }
    }

    /// The parent session, if this session was forked from another.
    pub fn parent_session(&self) -> &Option<SessionId> {
        &self.core.parent_session
    }

    /// The highest entry ordinal inherited from parent at fork time.
    /// `None` for root sessions.
    pub fn fork_ordinal(&self) -> Option<usize> {
        self.core.fork_ordinal
    }

    /// How this session came into being. Identity, not structure —
    /// see [`SessionOrigin`].
    pub fn origin(&self) -> SessionOrigin {
        self.core.origin
    }

    /// Set the fork ordinal for testing and construction.
    pub fn set_fork_ordinal(&mut self, ordinal: usize) {
        self.core.fork_ordinal = Some(ordinal);
    }

    /// Set the parent session.
    pub fn set_parent_session(&mut self, parent: SessionId) {
        self.core.parent_session = Some(parent);
    }

    /// The cached context size in tokens, if a prompt has been assembled.
    pub fn context_size(&self) -> Option<u32> {
        self.core.ephemeral.cached_context_size
    }

    /// Update the cached context size.
    pub fn set_context_size(&mut self, size: u32) {
        self.core.ephemeral.cached_context_size = Some(size);
    }

    // ----- Discovered resources (per-session, cwd-scoped) -----
    //
    // These are populated by the scan actors and read by prompt assembly and
    // the skill tool. They are NOT persisted — see `.plans/project-locals/plan.md`
    // decision D3 for the per-session isolation rationale.

    /// Returns the skills discovered for this session's cwd tree.
    pub fn discovered_skills(&self) -> &[crate::feat::skills::Skill] {
        &self.core.ephemeral.discovered_skills
    }

    /// Returns the prompt templates discovered for this session's cwd tree.
    pub fn discovered_prompt_templates(
        &self,
    ) -> &crate::feat::context::prompt_template::PromptTemplateStore {
        &self.core.ephemeral.discovered_prompt_templates
    }

    /// Returns the context files discovered for this session's cwd tree.
    pub fn discovered_context_files(&self) -> &[crate::feat::context::env_context::ContextFile] {
        &self.core.ephemeral.discovered_context_files
    }

    /// Replaces the discovered skills set for this session (scan-actor write path).
    pub fn set_discovered_skills(&mut self, skills: Vec<crate::feat::skills::Skill>) {
        self.core.ephemeral.discovered_skills = skills;
    }

    /// Replaces the discovered prompt-template store for this session.
    pub fn set_discovered_prompt_templates(
        &mut self,
        store: crate::feat::context::prompt_template::PromptTemplateStore,
    ) {
        self.core.ephemeral.discovered_prompt_templates = store;
    }

    /// Replaces the discovered context files for this session.
    pub fn set_discovered_context_files(
        &mut self,
        files: Vec<crate::feat::context::env_context::ContextFile>,
    ) {
        self.core.ephemeral.discovered_context_files = files;
    }

    /// Restore the token ledger from persisted data.
    pub fn restore_token_ledger(&mut self, records: Vec<TokenRecord>) {
        self.core.token_ledger = records;
    }

    /// Restore the parent session from persisted data.
    pub fn restore_parent_session(&mut self, parent: Option<SessionId>) {
        self.core.parent_session = parent;
    }

    /// Restore the updated_at timestamp from persisted data.
    pub fn restore_updated_at(&mut self, ts: jiff::Timestamp) {
        self.core.updated_at = ts;
    }

    /// Restore the creation timestamp from persisted data.
    pub fn restore_created_at(&mut self, ts: jiff::Timestamp) {
        self.core.created_at = ts;
    }

    /// This session's unique identifier.
    pub fn session_id(&self) -> &SessionId {
        &self.core.session_id
    }

    /// Set the session ID (used when inserting into a HashMap with an external key).
    pub fn set_session_id(&mut self, id: SessionId) {
        self.core.session_id = id;
    }

    /// The session title. `None` until the first user message.
    pub fn title(&self) -> Option<&str> {
        self.core.title.as_deref()
    }

    /// Set the session title.
    pub fn set_title(&mut self, title: String) {
        self.core.title = Some(title);
    }

    /// When this session was last updated.
    pub fn updated_at(&self) -> &Timestamp {
        &self.core.updated_at
    }

    /// When this session was created. Never changes after construction.
    pub fn created_at(&self) -> &Timestamp {
        &self.core.created_at
    }

    /// Update the timestamp to now.
    pub fn touch(&mut self) {
        self.core.updated_at = Timestamp::now();
    }

    /// Generic blob storage for future subsystems.
    pub fn blobs(&self) -> &HashMap<String, JsonValue> {
        &self.core.blobs
    }

    /// Mutable access to generic blob storage.
    pub fn blobs_mut(&mut self) -> &mut HashMap<String, JsonValue> {
        &mut self.core.blobs
    }

    /// The name of the lifecycle that created this session, if any.
    pub fn lifecycle_name(&self) -> Option<&str> {
        self.core.lifecycle_name.as_deref()
    }

    /// Set the lifecycle name.
    pub fn set_lifecycle_name(&mut self, name: Option<String>) {
        self.core.lifecycle_name = name;
    }

    /// The args used during setup (replayed for teardown).
    pub fn lifecycle_args(&self) -> &[String] {
        &self.core.lifecycle_args
    }

    /// Set the lifecycle args.
    pub fn set_lifecycle_args(&mut self, args: Vec<String>) {
        self.core.lifecycle_args = args;
    }

    /// Returns the session's memory state.
    pub fn session_state(&self) -> SessionState {
        self.core.session_state
    }

    /// Sets the session's memory state.
    pub fn set_session_state(&mut self, state: SessionState) {
        self.core.session_state = state;
    }

    /// Returns the lifecycle script state.
    pub fn lifecycle_script_state(&self) -> LifecycleScriptState {
        self.core.lifecycle_script_state
    }

    /// Advances lifecycle state after successful setup: `NothingRan → SetupRan`.
    pub fn advance_lifecycle_after_setup(&mut self) {
        self.core.lifecycle_script_state.advance_after_setup();
    }

    /// Advances lifecycle state after successful teardown: `SetupRan → TeardownRan`.
    pub fn advance_lifecycle_after_teardown(&mut self) {
        self.core.lifecycle_script_state.advance_after_teardown();
    }

    /// Force-exclude any `ToolCall` entries that lack matching `ToolResult` entries,
    /// and their empty parent `Assistant` entry.
    ///
    /// Called after hard cancel (ESC) to ensure the assembled prompt doesn't
    /// contain dangling `tool_calls` without corresponding `tool` results,
    /// which causes LLM providers to reject the request (e.g., ZAI error 1214).
    ///
    /// Uses `ContextOverride::ForcedExclude` rather than removing entries,
    /// preserving them for display in the UI.
    pub fn force_exclude_dangling_tool_calls(&mut self) -> Vec<ChatEntryId> {
        self.edit_history().exclude_incomplete_trailing_loops()
    }

    /// Disable the tool loop for this session's current turn.
    ///
    /// Delegates to [`SessionPhaseMachine::set_tool_loop_disabled`].
    pub fn set_tool_loop_disabled(&mut self) {
        self.core.ephemeral.machine.set_tool_loop_disabled();
    }

    /// Take the tool-loop-disabled flag, clearing it.
    ///
    /// Delegates to [`SessionPhaseMachine::take_tool_loop_disabled`].
    pub fn take_tool_loop_disabled(&mut self) -> bool {
        self.core.ephemeral.machine.take_tool_loop_disabled()
    }

    /// Check whether the tool loop is disabled, without clearing.
    ///
    /// Delegates to [`SessionPhaseMachine::is_tool_loop_disabled`].
    pub fn is_tool_loop_disabled(&self) -> bool {
        self.core.ephemeral.machine.is_tool_loop_disabled()
    }

    /// Resolve a [`ChatEntryId`] to its current index in history.
    ///
    /// Returns `None` if the entry no longer exists.
    pub fn find_entry_index_by_id(&self, id: &ChatEntryId) -> Option<usize> {
        self.core.history.iter().position(|e| e.id == *id)
    }

    /// Queue a batch of mutations for deferred application.
    ///
    /// Empty batches are silently ignored.
    pub fn queue_mutations(
        &mut self,
        batch: Vec<crate::feat::session::history_mutation::HistoryMutation>,
    ) {
        if !batch.is_empty() {
            self.core.ephemeral.pending_mutations.push(batch);
        }
    }

    /// Drain all pending mutation batches.
    pub fn drain_pending_mutations(
        &mut self,
    ) -> Vec<Vec<crate::feat::session::history_mutation::HistoryMutation>> {
        std::mem::take(&mut self.core.ephemeral.pending_mutations)
    }

    /// Apply a batch of mutations. Resolves IDs to current positions.
    ///
    /// Silently skips mutations targeting nonexistent entries.
    /// Processing order within a batch is preserved - earlier mutations
    /// are visible to later ones in the same batch.
    pub fn apply_mutations(
        &mut self,
        batch: Vec<crate::feat::session::history_mutation::HistoryMutation>,
    ) -> Vec<ChatEntryId> {
        self.edit_history().apply(batch)
    }

    /// Drain all pending mutation batches and apply them.
    ///
    /// Returns the number of batches applied and the entry IDs whose
    /// `context_override` actually changed value during this drain.
    pub fn drain_and_apply_pending_mutations(&mut self) -> (usize, Vec<ChatEntryId>) {
        let batches = self.drain_pending_mutations();
        let count = batches.len();
        let mut changed = Vec::new();
        for batch in batches {
            let mut batch_changed = self.apply_mutations(batch);
            changed.append(&mut batch_changed);
        }
        (count, changed)
    }

    /// Current deduplicated token total held in the accumulation buffer.
    ///
    /// Drives the threshold flush decision in `handle_submit_history_mutations`.
    #[must_use]
    pub fn accumulated_overrides_total(&self) -> u64 {
        self.core.ephemeral.accumulated_overrides.total_tokens()
    }

    /// The number of distinct entries currently held back in the accumulation
    /// buffer awaiting the threshold flush. Each represents one pending prune.
    /// Unlike the token total, this is stable across threshold changes and
    /// counts only `ForcedExclude` (prune) overrides — shields and compaction
    /// never enter the buffer.
    #[must_use]
    pub fn accumulated_prune_count(&self) -> usize {
        self.core.ephemeral.accumulated_overrides.len()
    }

    /// Pushes a `SetContextOverride` mutation into the accumulation buffer with
    /// its pre-resolved token cost. The buffer dedups by entry and respects
    /// shield dominance; the override is held back until the threshold flush.
    pub fn route_override(
        &mut self,
        entry_id: ChatEntryId,
        value: crate::feat::session::chat_entry::ContextOverride,
        source: crate::feat::session::chat_entry::ChangeSource,
        token_cost: u32,
    ) {
        self.core
            .ephemeral
            .accumulated_overrides
            .push(entry_id, value, source, token_cost);
    }

    /// Drains the accumulation buffer into `pending_mutations` as a single batch
    /// if its token total has crossed the threshold.
    ///
    /// Returns `true` if a flush occurred.
    pub fn flush_accumulated_overrides_if_needed(&mut self, threshold: u32) -> bool {
        if self.accumulated_overrides_total() >= u64::from(threshold) {
            let batch = self.core.ephemeral.accumulated_overrides.drain();
            self.queue_mutations(batch);
            true
        } else {
            false
        }
    }
}

/// If `entry` is a [`ChatEntryKind::User`] entry, expand `#name` prompt-template
/// tokens in its `display` against `store` and write the result to `expanded`.
/// `display` is left untouched so the UI keeps the raw token text while the model
/// receives the expanded prompt body. Non-`User` kinds are unchanged.
///
/// This is the single expansion site for all user entries — see
/// [`ChatSessionState::push_entry`].
pub(crate) fn expand_user_entry(
    entry: &mut ChatEntry,
    store: &PromptTemplateStore,
    ctx: &PathResolveContext<'_>,
) -> Vec<PendingPath> {
    let mut pending_paths = Vec::new();
    if let ChatEntryKind::User {
        display,
        expanded,
        outcome,
        ..
    } = &mut entry.kind
    {
        // Pass 1: `#token` template expansion.
        let token_expanded = expand_tokens(display, store);
        // Pass 2: `@path` token rewriting — resolves paths against the session
        // cwd/home and rewrites each attachable `@path` to a `file://` URI.
        // Tokens previously marked degraded (missing file or not-an-image)
        // via [`ChatEntryKind::User::outcome`] are left as their original literal
        // text, which makes re-expansion idempotent: a resolved entry pushed
        // a second time (e.g. via the queue path) keeps its literal `@path`
        // tokens instead of being rewritten back to `(file://…)` links. This
        // is a pure-text transform; reading image bytes / classifying /
        // filling `attachments` happens in the async session actor (see
        // `handle_enqueue_user_message`), so blocking I/O stays in
        // `spawn_blocking`. The resolved paths are returned to the actor for
        // the async byte-reading + conversion phase.
        let degraded_raw: Vec<String> = outcome.degraded.iter().map(|t| t.raw.clone()).collect();
        let scanned = scan_at_paths_with_degraded(&token_expanded, ctx, &degraded_raw);
        *expanded = scanned.rewritten_text;
        pending_paths = scanned.pending_paths;
    }
    pending_paths
}

impl Default for ChatSessionState {
    fn default() -> Self {
        Self::new()
    }
}

/// Builder for constructing [`ChatSessionState`] in tests.
///
/// Replays operations sequentially on `build()`. Example:
///
/// ```ignore
/// let mut session = ChatSessionState::builder()
///     .with_user_entry("hello")
///     .begin_streaming()
///     .build();
/// session.append_stream_token("world");
/// ```
#[cfg(test)]
#[must_use]
#[derive(Debug, Default)]
pub struct ChatSessionStateBuilder {
    ops: Vec<BuilderOp>,
}

#[cfg(test)]
#[derive(Debug)]
enum BuilderOp {
    PushEntry(Box<ChatEntry>),
    BeginStreaming,
    BeginSending,
    PinLast(PinPosition),
}

#[cfg(test)]
impl ChatSessionStateBuilder {
    /// Push a user entry onto the history.
    pub fn with_user_entry(mut self, text: &str) -> Self {
        self.ops
            .push(BuilderOp::PushEntry(Box::new(ChatEntry::user(text))));
        self
    }

    /// Push any entry onto the history.
    pub fn with_entry(mut self, entry: ChatEntry) -> Self {
        self.ops.push(BuilderOp::PushEntry(Box::new(entry)));
        self
    }

    /// Begin streaming (creates an empty Assistant entry and sets `is_streaming`).
    pub fn begin_streaming(mut self) -> Self {
        self.ops.push(BuilderOp::BeginStreaming);
        self
    }

    /// Mark the session as sending.
    pub fn begin_sending(mut self) -> Self {
        self.ops.push(BuilderOp::BeginSending);
        self
    }

    /// Pin the most recently pushed entry at the given position.
    pub fn with_pin(mut self, position: PinPosition) -> Self {
        self.ops.push(BuilderOp::PinLast(position));
        self
    }

    /// Build the session by replaying all stored operations.
    pub fn build(self) -> ChatSessionState {
        let mut session = ChatSessionState::new();
        let mut last_id: Option<ChatEntryId> = None;
        for op in self.ops {
            match op {
                BuilderOp::PushEntry(entry) => {
                    let entry = *entry;
                    let id = entry.id.clone();
                    session.push_entry(entry);
                    last_id = Some(id);
                }
                BuilderOp::BeginStreaming => {
                    session.begin_streaming();
                }
                BuilderOp::BeginSending => {
                    session.begin_sending();
                }
                BuilderOp::PinLast(position) => {
                    if let Some(ref id) = last_id {
                        session.pin_entry(id, position);
                    }
                }
            }
        }
        session
    }
}

#[cfg(test)]
impl ChatSessionState {
    /// Create a test builder.
    pub fn builder() -> ChatSessionStateBuilder {
        ChatSessionStateBuilder::default()
    }
}

#[cfg(test)]
mod chat_session_tests;
