//! Chat session protocol - state types for a single conversation.
//!
//! [`ChatSessionState`] owns the history and streaming state for one chat session.
//! Multiple sessions can exist concurrently in the application, each identified
//! by a [`SessionId`](jinn_core_types::SessionId).
//!
//! Fields are grouped into [`SessionCore`] (session-actor / context-actor)
//! and [`SessionUi`] (IntentHandler) sub-structs to make cross-boundary
//! writes visually obvious during code review.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::atomic::Ordering;

use jiff::Timestamp;
use jinn_bot_msg::{BotBehavior, BotModelSetting, BotReport, BotTrigger};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use jinn_chat_log_view_msg::visual_item::{
    DEFAULT_MIN_COLLAPSE_COUNT, PROXIMITY_COUNT, VisualItem, build_visual_items,
    entry_id_from_visual_item, resolve_entry_id_to_vi_index,
};
use jinn_context::attachment_path::{PathResolveContext, PendingPath, scan_at_paths_with_degraded};
use jinn_context::{PromptTemplateStore, expand_tokens};
use jinn_core_types::model_selection::ModelSelection;
use jinn_core_types::{
    ChangeSource, ChatEntry, ChatEntryId, ChatEntryKind, ContextOverride, EntryTiming,
    HistoryMutation, NameFilter, PinPosition, SessionId, ToolCallId, ToolResultStatus,
};
use jinn_session_history::history_editor::{
    Chunk, HistoryEditor, Priv, SessionHistoryAccess, SessionHistoryAccessPriv, chunk_containing,
    chunking,
};
use jinn_session_store_msg::SessionState;
use jinn_skills_msg::Skill;
use jinn_skills_msg::loaded_skill_summary_label;
use jinn_skills_msg::parse_loaded_skill_name;
use jinn_slices::PhaseView;
use jinn_token_count_msg::TokenRecord;
use jinn_workflows_msg::Workflow;
use marked_path::{Absolute, MarkedPathBuf};

use jinn_chat_log_view_msg::SavedHistoryPosition;
use jinn_session_lifecycle_msg::LifecycleScriptState;
use jinn_session_msg::SessionOrigin;

use crate::core::SessionCore;
use crate::fields::{LifecycleArgs, SessionProfile};
use crate::runtime::SessionUi;
use crate::steering_buffer::SteeringBuffer;

mod bot;
mod enablement;
mod facades;
mod history_edits;
mod identity;
mod mutations;
mod pins;
mod records;
mod selection;
mod streaming;
mod tool_calls;
mod viewport;

// Local types that moved into child modules keep their public paths here.
pub use pins::{PinDisplayRow, PinRowLabel};
pub use streaming::StreamingError;

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
#[expect(
    clippy::partial_pub_fields,
    reason = "UI state remains explicitly public while the authoritative core stays private"
)]
#[derive(Debug, Serialize, Deserialize)]
pub struct ChatSessionState {
    /// Core domain state managed by session-actor and context-actor.
    #[serde(flatten)]
    core: SessionCore,
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
    slices: std::sync::OnceLock<jinn_slices::Slices>,
    /// In-struct stand-in for this session's view state while
    /// `slices` is unattached. Reads see it, writes mutate it, so an
    /// unattached configuration behaves exactly like a session with the
    /// handle attached.
    /// Ignored entirely once the handle is attached.
    #[serde(skip)]
    view_fallback: parking_lot::RwLock<jinn_chat_log_view_msg::ChatLogViewUi>,
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
        }
    }

    /// Opens the sole write path to this session's history.
    ///
    /// All history mutations go through the returned [`HistoryEditor`]; reads
    /// stay on the session itself.
    pub fn edit_history(&mut self) -> HistoryEditor<'_, Self> {
        HistoryEditor::new(self)
    }

    /// Create a new session with a specific profile (model + strategy).
    #[must_use]
    pub fn new_with_profile(profile: SessionProfile) -> Self {
        let mut core = SessionCore::default();
        core.integrations.profile = profile;
        Self {
            core,
            ui: SessionUi::default(),
            slices: std::sync::OnceLock::new(),
            view_fallback: parking_lot::RwLock::new(
                jinn_chat_log_view_msg::ChatLogViewUi::default(),
            ),
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
        let mut core = SessionCore::default();
        core.identity.parent_session = Some(parent_session_id.clone());
        core.identity.origin = SessionOrigin::Subagent;
        core.storage.persist = persist;
        Self {
            core,
            ui: SessionUi::default(),
            slices: std::sync::OnceLock::new(),
            view_fallback: parking_lot::RwLock::new(
                jinn_chat_log_view_msg::ChatLogViewUi::default(),
            ),
        }
    }

    /// Create a bot session: a peer that references a parent without
    /// inheriting its conversation.
    ///
    /// Copies the parent's environment — profile, cwd, project, home, and
    /// enabled MCP servers — and links via `parent_session`. History starts
    /// empty and in prep mode, so the user can compose its instructions
    /// before anything dispatches.
    ///
    /// Does not reuse [`new_child`](Self::new_child): that constructor
    /// hard-codes the `Subagent` origin, and a bot is a different
    /// creation path.
    #[must_use]
    pub fn new_bot(parent: &Self, persist: bool) -> Self {
        {
            let mut bot = Self {
                core: SessionCore::default(),
                ui: SessionUi::default(),
                slices: std::sync::OnceLock::new(),
                view_fallback: parking_lot::RwLock::new(
                    jinn_chat_log_view_msg::ChatLogViewUi::default(),
                ),
            };
            let bot_core = &mut bot.core;
            bot_core.identity.parent_session = Some(parent.core.identity.session_id.clone());
            bot_core.identity.origin = SessionOrigin::Bot;
            bot_core
                .identity
                .project
                .clone_from(&parent.core.identity.project);
            bot_core.storage.persist = persist;
            bot_core.integrations.profile = parent.core.integrations.profile.clone();
            bot_core
                .lifecycle
                .cwd
                .clone_from(&parent.core.lifecycle.cwd);
            bot_core
                .lifecycle
                .home
                .clone_from(&parent.core.lifecycle.home);
            bot_core
                .integrations
                .enabled_mcp_servers
                .clone_from(&parent.core.integrations.enabled_mcp_servers);
            bot
        }
    }

    /// Creates a standalone bot: a bot with no parent, inheriting the
    /// *environment* of `source` but nothing else about it.
    ///
    /// A standalone bot is the same session kind as [`Self::new_bot`] —
    /// origin [`SessionOrigin::Bot`], prep mode on, the same
    /// copy-the-environment rule — minus the parent link. Its seed
    /// template starts empty, because the parent line an attached bot's
    /// default names is a line a parentless bot cannot fill, and the
    /// dispatched message is meant to be its whole prompt.
    ///
    /// `source` supplies the project, profile, cwd, home, and enabled MCP
    /// servers exactly as a parent would; the conversation, pins, and bot
    /// configuration of the source are not carried over.
    #[must_use]
    pub fn new_standalone_bot(source: &Self, persist: bool) -> Self {
        let mut bot = Self::new_bot(source, persist);
        bot.core.identity.parent_session = None;
        bot.core.bot.seed_template = jinn_bot_msg::empty_seed_template();
        bot
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
        &self.core.integrations.profile.persona_name
    }

    /// Set the session's persona name.
    pub fn set_persona_name(&mut self, name: String) {
        self.core.integrations.profile.persona_name = name;
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
        let (cwd_view, home_view) = (
            self.core.lifecycle.cwd.as_marked_path(),
            self.core.lifecycle.home.as_marked_path(),
        );
        let ctx = PathResolveContext::new(&cwd_view, &home_view);
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
    #[expect(
        clippy::same_name_method,
        reason = "trait impl delegates to this inherent method; callers use both"
    )]
    pub fn insert_entry_at(&mut self, index: usize, entry: ChatEntry) -> usize {
        let clamped = index.min(self.core.history_work.history.len());
        self.core.history_work.history.insert(clamped, entry);
        // Shift every live index the insert displaced (handles all 4 streaming fields).
        self.core
            .ephemeral
            .shift_streaming_indices_for_insert_at(clamped);
        clamped
    }

    /// Returns the current session lifecycle phase.
    ///
    /// A display projection: written by the turn ledger's admission and
    /// settlement paths. Admission never reads it — the ledger decides.
    #[must_use]
    pub fn phase(&self) -> PhaseView {
        self.core.ephemeral.phase
    }

    /// Read-only access to the session profile.
    pub fn profile(&self) -> &SessionProfile {
        &self.core.integrations.profile
    }

    /// Mutable access to the session profile.
    pub fn profile_mut(&mut self) -> &mut SessionProfile {
        &mut self.core.integrations.profile
    }

    /// The model selection for this session.
    pub fn model_selection(&self) -> &ModelSelection {
        &self.core.integrations.profile.model
    }
    pub fn model(&self) -> &ModelSelection {
        &self.core.integrations.profile.model
    }

    /// Resolve a [`ChatEntryId`] to its current index in history.
    ///
    /// Returns `None` if the entry no longer exists.
    pub fn find_entry_index_by_id(&self, id: &ChatEntryId) -> Option<usize> {
        self.core
            .history_work
            .history
            .iter()
            .position(|e| e.id == *id)
    }
}

/// If `entry` is a [`ChatEntryKind::User`] entry, expand `#name` prompt-template
/// tokens in its `display` against `store` and write the result to `expanded`.
/// `display` is left untouched so the UI keeps the raw token text while the model
/// receives the expanded prompt body. Non-`User` kinds are unchanged.
///
/// This is the single expansion site for all user entries — see
/// [`ChatSessionState::push_entry`].
pub fn expand_user_entry(
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

impl SessionHistoryAccessPriv for ChatSessionState {
    fn seal(&self) -> Priv {
        Priv::construct()
    }
}

impl SessionHistoryAccess for ChatSessionState {
    fn history(&self) -> &[ChatEntry] {
        &self.core.history_work.history
    }

    fn push_entry_raw(&mut self, entry: &mut ChatEntry) -> usize {
        self.push_entry_raw(entry)
    }

    fn history_get_mut(&mut self, index: usize) -> Option<&mut ChatEntry> {
        self.history_get_mut(index)
    }

    fn insert_entry_at(&mut self, index: usize, entry: ChatEntry) -> usize {
        self.insert_entry_at(index, entry)
    }

    fn remove_history_entry_at(&mut self, index: usize) -> bool {
        self.remove_history_entry_at(index)
    }
}
