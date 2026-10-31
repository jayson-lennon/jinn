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

////// The tool-result content for a call a stream rule interrupted mid-arguments.
///
/// The content is what the model reads instead of running the call, so it has
/// to say two things: that the call did not happen, and what it was trying to
/// do. The arguments ride verbatim and are labelled truncated — completing them
/// would invent a call the model never made, and the whole point is to show
/// the model the mistake it is about to repeat.
///
/// The rule is named only as a user-defined rule, never by name: the name
/// crosses no boundary, and a generic framing keeps the result about the call
/// rather than about jinn's configuration.
///
/// The fragment lives in the result's content rather than the call's arguments
/// deliberately. Arguments are serialized arguments that every provider parses
/// as JSON, and a truncated fragment is not JSON — Google's encoder degrades
/// one to `{}` silently, which would leave the model a call that does nothing
/// and no explanation of why. Result content is free text on all three
/// providers, so this text reaches the model intact everywhere.
fn interrupted_call_result(tool_name: &str, arguments: &str) -> String {
    format!(
        "This `{tool_name}` call did not run. A user-defined rule triggered \
     and stopped the response before the call was sent.\n\n\
     The tool call fragment provided (do not re-run as is), truncated where it was interrupted:\n\n\
     {arguments}"
    )
}

/// The one place an absent filter is answered at a gate.
///
/// An absent filter inherits, and an inherited filter withholds nothing, so
/// the answer is the same as an empty deny filter's — but the decision is
/// made here, from the field's `None`, rather than inside the filter, whose
/// emptiness means nothing at all.
fn permits_or_inherits(filter: Option<&NameFilter>, name: &str) -> bool {
    filter.as_ref().is_none_or(|filter| filter.permits(name))
}

/// Stamps an entry with the dispatching generation's requested model.
///
/// Both fields get the same value: `model_requested` is the generation's
/// request-time fact and is never overridden, while `model_used` starts
/// here and is later replaced by the response-reported model when the
/// stream completes normally. Tool results are deliberately unstamped —
/// they are produced locally by the tool executor after the stream ended;
/// the audit popup derives their model from the paired tool call instead.
fn stamp_entry_with_generation_model(entry: &mut ChatEntry, model: Option<&str>) {
    entry.model_used = model.map(str::to_owned);
    entry.model_requested = model.map(str::to_owned);
}

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
    /// Writers get-or-insert their session's entry.
    ///
    /// A no-op when the cell is absent (handle unattached, or the catalog
    /// never ran). The draft the cell holds for this session is the only
    /// copy — there is no second one on the session struct to fall back to,
    /// so a missing cell means there is no draft to write.
    pub fn update_input<F>(&self, f: F)
    where
        F: FnOnce(&mut jinn_chat_input_msg::ChatInputBoxState),
    {
        if let Some(cell) = self.input_cell() {
            let id = self.session_id().clone();
            cell.update(|inputs| f(inputs.entry(id).or_default()));
        }
    }

    /// Reads this session's input draft through `f`, falling back to
    /// `default` when the cell is absent (handle unattached, or the catalog
    /// never ran) or when the session has no entry. Readers never grow the
    /// map: a session with no entry reads as its default draft.
    pub fn with_input<R, F, D>(&self, f: F, default: D) -> R
    where
        F: FnOnce(&jinn_chat_input_msg::ChatInputBoxState) -> R,
        D: FnOnce() -> R,
    {
        let Some(cell) = self.input_cell() else {
            return default();
        };
        let inputs = cell.read();
        let id = self.session_id();
        match inputs.get(id) {
            Some(input) => f(input),
            None => default(),
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

    /// A snapshot copy of the expanded entry ids.
    ///
    /// The off-thread chat log layout reads the set once per job rather than
    /// testing membership per entry, matching
    /// [`Self::streaming_tool_call_ids`]'s single-snapshot approach.
    #[must_use]
    pub fn expanded_entry_ids(&self) -> std::collections::HashSet<ChatEntryId> {
        self.with_view(|v| v.expanded_entries.clone(), Default::default)
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
    #[expect(
        clippy::same_name_method,
        reason = "trait impl delegates to this inherent method; callers use both"
    )]
    pub(crate) fn push_entry_raw(&mut self, entry: &mut ChatEntry) -> usize {
        self.core.identity.last_history_activity_at = Timestamp::now();
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
        let cursor_at_last = self.with_view(
            |v| {
                v.selected_cursor_id.as_ref().is_none_or(|id| {
                    self.core
                        .history_work
                        .history
                        .last()
                        .is_some_and(|e| &e.id == id)
                })
            },
            || true,
        );
        let index = self.core.history_work.history.len();
        self.core.history_work.history.push(entry.clone());
        if cursor_at_last {
            self.scroll_to_bottom();
            if let Some(entry) = self.core.history_work.history.last() {
                let id = entry.id.clone();
                self.update_view(|v| v.selected_cursor_id = Some(id));
            }
        }
        index
    }

    /// Removes the history entry at `index`. Returns whether it existed.
    ///
    /// Editor-only. Callers must remove in descending index order.
    ///
    /// Every live streaming index above `index` is shifted down by one, because
    /// the removal moves those entries with it. Skipping this would silently
    /// aim the next argument delta at whatever entry slid into the vacated
    /// slot — the delta is accepted, so nothing reports the mistake.
    #[expect(
        clippy::same_name_method,
        reason = "trait impl delegates to this inherent method; callers use both"
    )]
    pub(crate) fn remove_history_entry_at(&mut self, index: usize) -> bool {
        if index < self.core.history_work.history.len() {
            self.core.history_work.history.remove(index);
            self.core
                .ephemeral
                .shift_streaming_indices_after_remove_at(index);
            true
        } else {
            false
        }
    }

    /// Mutable access to the history entry at `index` for the editor.
    ///
    /// In-place writes (streaming lifecycle) can never reorder entries or
    /// split a tool loop, so the editor exposes them without chunk logic.
    #[expect(
        clippy::same_name_method,
        reason = "trait impl delegates to this inherent method; callers use both"
    )]
    pub(crate) fn history_get_mut(&mut self, index: usize) -> Option<&mut ChatEntry> {
        self.core.history_work.history.get_mut(index)
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

    /// Whether this session is a bot of another session.
    #[must_use]
    pub fn is_bot(&self) -> bool {
        self.core.identity.origin == SessionOrigin::Bot
    }

    /// What a run in this session sees of the conversation.
    #[must_use]
    pub fn bot_behavior(&self) -> BotBehavior {
        self.core.bot.behavior
    }

    /// Set what a run in this session sees of the conversation.
    pub fn set_bot_behavior(&mut self, behavior: BotBehavior) {
        self.core.bot.behavior = behavior;
    }

    /// Whether this bot is still being composed.
    ///
    /// Composition is the only configuration that stops a dispatch, and it
    /// says so on its own: a manual trigger stops a *fire*, not the user
    /// pressing `R`, so conflating the two would mark a perfectly runnable
    /// bot as stopped. Everything else runs on its own terms.
    ///
    /// Only meaningful for a bot — an ordinary session carries the
    /// same default fields and would answer `true`.
    #[must_use]
    pub fn bot_is_prepping(&self) -> bool {
        self.core.bot.prep_mode
    }

    /// Set whether this bot is still being composed.
    pub fn set_bot_is_prepping(&mut self, prep_mode: bool) {
        self.core.bot.prep_mode = prep_mode;
    }

    /// Whether this bot runs on its parent's completion.
    ///
    /// The sidebar marks this with its own glyph. It is a fact about the
    /// configuration rather than a derived state, so it is asked of the
    /// session rather than re-derived by each renderer — a marker whose
    /// meaning is a few fields away is a marker the two surfaces can
    /// disagree about.
    #[must_use]
    pub fn bot_fires_on_parent_completion(&self) -> bool {
        self.bot_trigger() == BotTrigger::ParentCompleted
    }

    /// The condition that causes an automatic re-run.
    #[must_use]
    pub fn bot_trigger(&self) -> BotTrigger {
        self.core.bot.trigger
    }

    /// Set the condition that causes an automatic re-run.
    pub fn set_bot_trigger(&mut self, trigger: BotTrigger) {
        self.core.bot.trigger = trigger;
    }

    /// The user-editable seed text injected ahead of the prior report.
    #[must_use]
    pub fn seed_template(&self) -> &str {
        &self.core.bot.seed_template
    }

    /// Set the user-editable seed text injected ahead of the prior report.
    pub fn set_seed_template(&mut self, template: String) {
        self.core.bot.seed_template = template;
    }

    /// Whether this bot's model is its own or the one it inherited.
    ///
    /// A fact about the bot's configuration rather than a reading of
    /// its model: the model is always concrete, and this says whether the
    /// bot claimed it. Never infer one from the other.
    #[must_use]
    pub fn bot_model_setting(&self) -> BotModelSetting {
        self.core.bot.model_setting
    }

    /// Set whether this bot's model is its own or inherited.
    pub fn set_bot_model_setting(&mut self, setting: BotModelSetting) {
        self.core.bot.model_setting = setting;
    }

    /// The bot's append-only report log, oldest first.
    #[must_use]
    pub fn bot_reports(&self) -> &[BotReport] {
        &self.core.bot.reports
    }

    /// Append one report to this bot's log.
    ///
    /// Append-only by design: the harness never removes or edits a report,
    /// and the next run is seeded from the most recent entry.
    pub fn append_bot_report(&mut self, body: String) -> BotReport {
        let reports = &mut self.core.bot.reports;
        let report = BotReport {
            run: reports.len() + 1,
            published_at: Timestamp::now(),
            body,
        };
        reports.push(report.clone());
        report
    }

    /// The most recent report, if this bot has ever reported.
    #[must_use]
    pub fn latest_bot_report(&self) -> Option<&BotReport> {
        self.core.bot.reports.last()
    }

    /// The workflow-local TOML file this bot was loaded from, when
    /// it belongs to a workflow. `None` for personal bots.
    #[must_use]
    pub fn workflow_source(&self) -> Option<&jinn_core_types::BotSourcePath> {
        self.core.bot.workflow_source.as_ref()
    }

    /// Stamp this bot as loaded from the workflow-local file `source`.
    pub fn set_workflow_source(&mut self, source: jinn_core_types::BotSourcePath) {
        self.core.bot.workflow_source = Some(source);
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

    /// Read-only access to the conversation history.
    #[expect(
        clippy::same_name_method,
        reason = "trait impl delegates to this inherent method; callers use both"
    )]
    pub fn history(&self) -> &[ChatEntry] {
        &self.core.history_work.history
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
        for entry in self.core.history_work.history.iter_mut() {
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
            if let Some(entry) = self.core.history_work.history.get_mut(i) {
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
    /// [`is_in_context`]: ChatEntry::is_in_context
    ///
    /// Returns `Some(entry_id)` if the override was changed, `None` if no-op
    /// (entry was already in the toggled state) or no entry is selected.
    pub fn toggle_entry_ignored(&mut self) -> Option<ChatEntryId> {
        let hist_idx = self.selected_history_index()?;
        let entry = self.core.history_work.history.get(hist_idx)?;
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
    ) -> Option<ChatEntryId> {
        let hist_idx = self.selected_history_index()?;
        let entry = self.core.history_work.history.get(hist_idx)?;
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
        let Some(idx) = self
            .core
            .history_work
            .history
            .iter()
            .position(|e| e.id == *entry_id)
        else {
            return;
        };

        // Scan backward to find the containing block's start.
        let mut block_start = idx;
        while block_start > 0 {
            let Some(prev) = self.core.history_work.history.get(block_start - 1) else {
                break;
            };
            if prev.is_in_context() || prev.pin_position.is_some() {
                break;
            }
            block_start -= 1;
        }

        let Some(block_entry) = self.core.history_work.history.get(block_start) else {
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
        let Some(forward_entry) = self.core.history_work.history.get(forward_start) else {
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
        let shown = self.shown_ignored_blocks_snapshot();
        let items = build_visual_items(
            &self.core.history_work.history,
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

    /// Captures all durable state from this authoritative session revision.
    ///
    /// Call under the application state read lock. The revision and payload are
    /// derived from the same core reference, so metadata, persisted history,
    /// and token accounting cannot come from separate application versions.
    #[must_use]
    pub fn capture_snapshot(&self) -> crate::snapshot::SessionSnapshot {
        let revision = self.core.next_capture_revision();
        crate::snapshot::SessionSnapshot::from((revision, &self.core))
    }

    /// The store crate (`jinn-session-store`) serializes this into the
    /// session row's metadata blob. Returns a clone so the live session is
    /// never mutably borrowed by persistence.
    #[must_use]
    pub fn persistable_core(&self) -> SessionCore {
        self.core.clone()
    }

    /// Replace the core wholesale (load path).
    ///
    /// The store crate rebuilds a [`SessionCore`] from a persisted snapshot
    /// and swaps it into a default-constructed shell. Do not call on a live
    /// session — in-flight turn state lives in the core's `ephemeral`.
    pub fn set_core(&mut self, core: SessionCore) {
        self.core = core;
    }

    /// Whether this session has no history entries.
    ///
    /// A session is "empty" when it has never had any entries pushed -
    /// no user messages, no system messages, nothing.
    /// Not to be confused with [`Self::is_idle`] which checks
    /// streaming/sending/assembling state.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.core.history_work.history.is_empty()
    }

    /// Whether this session should be persisted to disk.
    pub fn persist(&self) -> bool {
        self.core.storage.persist
    }

    /// Mark this session as having been meaningfully interacted with by the user.
    /// Once set, the session becomes eligible for persistence.
    pub fn mark_interacted(&mut self) {
        self.core.identity.has_interacted = true;
    }

    /// Whether this session has been interacted with.
    #[must_use]
    pub fn has_interacted(&self) -> bool {
        self.core.identity.has_interacted
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
        if !self.core.storage.persist {
            return false;
        }
        if self.core.lifecycle.lifecycle_name.is_some() {
            return true;
        }
        if self.core.identity.parent_session.is_some() {
            return true;
        }
        if self.core.identity.has_interacted {
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

    /// Lazily create the Assistant entry for the current stream.
    ///
    /// Called on first `append_stream_token`, `finish_streaming`,
    /// `begin_tool_call`, or `cancel_streaming`. No-op if the entry
    /// already exists or the session is not streaming.
    fn ensure_assistant_entry(&mut self, dispatched_at: jiff::Timestamp) {
        if self.core.ephemeral.streaming_entry_index.is_some()
            || self.core.ephemeral.phase != PhaseView::Streaming
        {
            return;
        }
        let mut entry = ChatEntry::assistant("");
        entry.timing = EntryTiming::streamed(dispatched_at);
        entry.timing.set_first_token();
        stamp_entry_with_generation_model(&mut entry, self.stream_request_model().as_deref());
        let index = self.push_entry(entry);
        self.core.ephemeral.streaming_entry_index = Some(index);
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

    /// Mark a provider stream as begun (`Streaming`).
    ///
    /// A display projection: the turn ledger's mint paths call this after
    /// admission, and nothing reads it for admission. Setting the phase
    /// cannot fail — there is no validated machine left to refuse it — so
    /// tests that seed a busy session by hand get exactly what production's
    /// admitted mint would have written.
    pub fn begin_streaming(&mut self) {
        self.core.ephemeral.phase = PhaseView::Streaming;
        self.core.identity.last_history_activity_at = Timestamp::now();
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
        if self.core.ephemeral.phase != PhaseView::Streaming {
            tracing::warn!(
                current_phase = ?self.core.ephemeral.phase,
                "append_stream_token called while not streaming - ignoring"
            );
            return Err(StreamingError::NoStreamingEntry);
        }
        self.ensure_assistant_entry(dispatched_at);
        self.core.identity.last_history_activity_at = Timestamp::now();
        self.core.identity.last_provider_activity_at = Timestamp::now();
        let index = self
            .core
            .ephemeral
            .streaming_entry_index
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
        if self.core.ephemeral.phase != PhaseView::Streaming {
            tracing::warn!(
                current_phase = ?self.core.ephemeral.phase,
                "begin_thinking called while not streaming - ignoring"
            );
            return;
        }
        if self.core.ephemeral.streaming_thinking_entry_index.is_some() {
            tracing::warn!("begin_thinking called while already thinking - ignoring");
            return;
        }
        let mut entry = ChatEntry::thinking("");
        entry.timing = EntryTiming::streamed(dispatched_at);
        entry.timing.set_first_token();
        stamp_entry_with_generation_model(&mut entry, self.stream_request_model().as_deref());
        // Insert thinking BEFORE the assistant entry when the assistant entry
        // already exists. Some providers (OpenRouter) send reasoning tokens
        // AFTER content tokens, so the assistant entry is already in history.
        // The thinking entry should appear before it in the chat log.
        let index = if let Some(assistant_idx) = self.core.ephemeral.streaming_entry_index {
            self.insert_entry_at(assistant_idx, entry)
        } else {
            self.push_entry(entry)
        };
        self.core.ephemeral.streaming_thinking_entry_index = Some(index);
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
            .streaming_thinking_entry_index
            .ok_or(StreamingError::NoThinkingEntry)?;
        self.core.identity.last_history_activity_at = Timestamp::now();
        self.core.identity.last_provider_activity_at = Timestamp::now();
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
        self.core.ephemeral.streaming_thinking_entry_index
    }

    /// The index of the entry accumulating assistant content, if any.
    ///
    /// The renderer needs this to recognise the one entry whose content
    /// changes on every streamed token, as distinct from settled history.
    #[must_use]
    pub fn streaming_entry_index(&self) -> Option<usize> {
        self.core.ephemeral.streaming_entry_index
    }

    /// Settle the session to `Idle` from any phase.
    ///
    /// The display projection of a turn settlement: the turn ledger's
    /// settle paths call this after adjudicating the turn's end. Every
    /// registration the turn owned is dropped here with the phase — a
    /// settled session has no streaming indices and no tool tracking.
    pub fn settle_to_idle(&mut self) {
        self.core.ephemeral.phase = PhaseView::Idle;
        self.core.ephemeral.clear_streaming_indices();
    }

    /// Rewind for a re-dispatch of the live turn: `Streaming` → `Working`.
    ///
    /// Called by the stall-retry and rule-intercept paths, whose resumed
    /// dispatch re-enters `Streaming` through the ledger's mint. The
    /// streaming indices are dropped here, so the retried stream starts
    /// from a clean slate; the caller must have consumed them first.
    pub fn rewind_for_retry(&mut self) {
        self.core.ephemeral.phase = PhaseView::Working;
        self.core.ephemeral.streaming_entry_index = None;
        self.core.ephemeral.streaming_thinking_entry_index = None;
        self.core.ephemeral.clear_streaming_indices();
    }

    /// Sweeps every still-`Pending` tool result to `Failure`.
    ///
    /// The cancel drain's last step: a result whose call never completed
    /// (a straggler the synthesized completions missed, or one refused by
    /// the ledger) must not survive as `Pending` — an unresolved batch
    /// makes the context converter strip the whole assistant tool-call
    /// block (bug #21). Clearing the tracking indices keeps the machine
    /// consistent with the history it just settled.
    pub fn sweep_pending_tool_results_to_failure(&mut self) {
        let swept = self.edit_history().sweep_pending_tool_results_to_failure();
        self.core.ephemeral.streaming_tool_result_indices.clear();
        if swept > 0 {
            tracing::info!(
                swept,
                "swept pending tool results to Failure at cancel settle"
            );
        }
    }

    /// Finalize the streaming entries ahead of a normal (non-cancel)
    /// completion: ensure the assistant entry exists, stamp its
    /// `finished_at`, and resolve any still-pending thinking entry.
    ///
    /// The entry half of [`Self::settle_to_idle`], run by
    /// the session actor's completion fold before the phase actor applies
    /// the machine edge.
    ///
    /// Exclusively called by the session actor's completion fold.
    pub fn finalize_entries_for_finish(
        &mut self,
        preserve_assistant: bool,
        dispatched_at: jiff::Timestamp,
    ) {
        if preserve_assistant {
            self.ensure_assistant_entry(dispatched_at);
        }
        if let Some(idx) = self.core.ephemeral.streaming_entry_index {
            self.finish_streaming_entry(idx);
        }
        if let Some(idx) = self.core.ephemeral.streaming_thinking_entry_index {
            self.finish_thinking_entry(idx);
        }
    }

    /// Ends one streaming round within a live turn: the assistant and
    /// thinking entries are finalized, so their indices must not survive
    /// into the next round.
    ///
    /// Called by the session actor's completion fold after the entries are
    /// finalized and their models overridden. The old phase machine got
    /// this for free — the `Streaming` phase struct carrying the indices
    /// was dropped at the tool-use edge, so the next round lazily created
    /// fresh entries below the tool calls. The projection write that
    /// replaced the machine has no struct to drop, so the round boundary
    /// clears them explicitly. Keeping them would append the next round's
    /// tokens into the previous round's assistant entry, stacking every
    /// round's tool calls and results below a single stale text block.
    ///
    /// The tool-call and tool-result tracking maps are untouched: they are
    /// turn-scoped, and the batch the stream handed off to is still live.
    pub fn finish_streaming_round(&mut self) {
        self.core.ephemeral.streaming_entry_index = None;
        self.core.ephemeral.streaming_thinking_entry_index = None;
    }

    /// Cancel streaming but keep partial text in history.
    ///
    /// The entry half of a cancel: finalize the partial assistant entry
    /// and any still-pending thinking entry so their durations resolve.
    /// The machine edge is the phase actor's
    /// [`Self::settle_to_idle`], applied after this.
    ///
    /// Exclusively called by the session actor's completion fold.
    pub fn finalize_entries_for_cancel(&mut self, dispatched_at: jiff::Timestamp) {
        self.ensure_assistant_entry(dispatched_at);

        // Set finished_at on the assistant entry.
        if let Some(idx) = self.core.ephemeral.streaming_entry_index {
            self.finish_streaming_entry(idx);
        }

        // Safety net: finalize any still-pending thinking entry so its duration
        // resolves even if reasoning was interrupted. Must run before cancel()
        // drops the StreamingPhase and clears streaming_thinking_entry_index().
        if let Some(idx) = self.core.ephemeral.streaming_thinking_entry_index {
            self.finish_thinking_entry(idx);
        }
    }

    /// Sets the response-reported model on every live entry of the current
    /// generation, replacing only `model_used`.
    ///
    /// `model_requested` is never touched: it is the request-time fact and
    /// the whole point of the two fields is that they can differ. Live
    /// entries are the ones the machine's indices still name — the
    /// streaming assistant entry, the thinking entry, and every in-flight
    /// tool call. Entries created before the stream started (the user's
    /// message) are not provider-produced and are not stamped.
    ///
    /// Callers must invoke this while the indices are intact: the fold
    /// runs before the phase actor's settle drops the `StreamingPhase`.
    pub fn override_live_entry_models(&mut self, model: Option<&str>) {
        let indices = {
            let eph = &self.core.ephemeral;
            let mut all = Vec::new();
            if let Some(idx) = eph.streaming_entry_index {
                all.push(idx);
            }
            if let Some(idx) = eph.streaming_thinking_entry_index {
                all.push(idx);
            }
            all.extend(eph.streaming_tool_call_indices.values().copied());
            all
        };
        for idx in indices {
            self.edit_history().with_entry_at_mut(idx, |entry| {
                entry.model_used = model.map(str::to_owned);
            });
        }
    }

    /// Cancel streaming and drain steering fragments plus queued messages back
    /// into the input buffer.
    ///
    /// Used when the user interrupts via ESC-confirm during an active stream.
    /// Steering fragments (drained first) and the display text of each queued
    /// `UserMessage` are joined with `"\n\n---\n\n"` and replace whatever was
    /// in the input box. `ToolContinuation` items are silently discarded.
    /// If nothing was drained, the input box is left untouched.
    ///
    /// The drain half only: the phase edge is the phase actor's, applied
    /// when the cancel's `TurnCanceled` command lands.
    pub fn drain_cancelled_work_to_input(&mut self) {
        let drained_text = self.drain_cancel_chunks().join("\n\n---\n\n");
        if !drained_text.is_empty() {
            self.update_input(|input| input.replace_all(drained_text));
        }
    }

    /// The tool calls whose arguments are still streaming, as `(tool call id,
    /// tool name, arguments accumulated so far)`.
    ///
    /// The argument text is whatever the model had produced when this was
    /// read, which mid-stream is a prefix of the real JSON and usually not
    /// parseable. It is reported verbatim for exactly that reason: a fragment
    /// is worth showing to the model unaltered, where repairing it would
    /// invent content the model never wrote.
    ///
    /// Read from the streaming phase's index map, so this is only meaningful
    /// while a response is in flight — and only for calls whose arguments had
    /// started arriving. A model that opens with a tool call and is
    /// interrupted before its first delta appears here.
    #[must_use]
    pub fn streaming_tool_call_fragments(&self) -> Vec<(ToolCallId, String, String)> {
        let history = &self.core.history_work.history;
        self.core
            .ephemeral
            .streaming_tool_call_indices
            .values()
            .filter_map(|&index| history.get(index))
            .filter_map(|entry| match &entry.kind {
                ChatEntryKind::ToolCall {
                    id,
                    name,
                    arguments,
                    ..
                } => Some((id.clone(), name.clone(), arguments.clone())),
                _ => None,
            })
            .collect()
    }

    /// Complete the tool calls a stream rule interrupted, each paired with a
    /// synthetic result explaining that it never ran.
    ///
    /// An intercepted tool call is a call the model made and never got an
    /// answer for, because interrupting its argument stream means it is never
    /// dispatched. Leaving it unanswered is not a neutral outcome: providers
    /// reject a request whose `tool_calls` have no matching results, and
    /// force-excluding it leaves the model resuming with no memory of an
    /// attempt it will simply repeat. The synthetic result is what makes the
    /// call both valid and legible.
    ///
    /// The fragment is placed immediately after its call and *before* the
    /// interrupt's guidance entry, so the model reads the failure first and
    /// the advice second. Guidance is treated as skippable rather than
    /// terminating for the same reason it is not a provider message: it sits
    /// between a call and its result without breaking the pairing.
    ///
    /// Returns the number of calls completed. Zero means the intercept caught
    /// prose or reasoning rather than a tool call, which is the common case
    /// and needs nothing here.
    pub fn explain_interrupted_tool_calls(&mut self) -> usize {
        let fragments = self.streaming_tool_call_fragments();
        if fragments.is_empty() {
            return 0;
        }

        let history = self.core.history_work.history.clone();
        // Collected first and inserted back-to-front, so each insertion's
        // index stays valid: inserting in place would renumber the calls that
        // have not been paired yet.
        let mut insertions: Vec<(usize, ChatEntry)> = Vec::with_capacity(fragments.len());
        for (id, name, arguments) in fragments {
            let Some(call_index) = history.iter().position(
                |entry| matches!(&entry.kind, ChatEntryKind::ToolCall { id: t, .. } if t == &id),
            ) else {
                continue;
            };
            insertions.push((
                call_index + 1,
                ChatEntry::tool_result(
                    id.clone(),
                    name.clone(),
                    interrupted_call_result(&name, &arguments),
                    ToolResultStatus::Failure,
                ),
            ));
        }

        let completed = insertions.len();
        for (index, entry) in insertions.into_iter().rev() {
            self.insert_entry_at(index, entry);
        }
        completed
    }

    /// Prepare a stalled stream for retry: take the partial entries out of
    /// context and clear the streaming bookkeeping so the retried generation
    /// starts from scratch.
    ///
    /// A call a stream rule interrupted is deliberately left *in* context when
    /// it has been paired with a synthetic result: that pairing is what makes
    /// the resumed request valid, and taking the call out of context would
    /// leave an answered-but-absent call the model cannot read. Excluding it
    /// was the old behaviour and it is what made a model resume blind and
    /// re-emit the identical call.
    ///
    /// Nothing is removed. A stalled attempt's partial assistant text, thinking
    /// and tool calls stay in history exactly where the user watched them
    /// appear, marked `ForcedExclude` so the retried prompt stays valid —
    /// providers reject a request whose `tool_calls` have no matching results.
    /// This mirrors what the hard-cancel path already does with
    /// [`Self::force_exclude_dangling_tool_calls`].
    ///
    /// Must run while still in `Streaming`: it reads the streaming indices,
    /// which no longer exist once the phase has moved on.
    ///
    /// The excluded entries are also registered as an expanded ignored block.
    /// Exclusion alone is not enough to keep them readable: the chat log
    /// collapses any contiguous run of `!is_in_context()` entries that is at
    /// least `min_collapse_count` long and further than `proximity_count` from
    /// the tail, and it cannot tell a discarded stall from a block the user
    /// chose to ignore. Without this the whole attempt — three entries in the
    /// common case, exactly the collapse threshold — reduces to a single "N
    /// hidden entries" line, which is the invisibility this method exists to
    /// prevent. Registering the ids is a default-expanded state, not a lock:
    /// toggling the block collapses it like any other.
    ///
    /// Returns the ids whose context override changed.
    pub fn reset_streaming_entries_for_retry(&mut self) -> Vec<ChatEntryId> {
        // Collect first: clearing the indices below is what erases the only
        // record of which entries this generation owned.
        let mut indices = self.collect_streaming_history_indices();
        indices.sort_unstable();
        indices.dedup();
        // A tool call answered by a synthetic result is a finished exchange,
        // not an abandoned attempt: it and its result stay in context so the
        // model can read them. Dropping the call would strand an answered
        // call the model never sees, which is the blind resume this whole
        // method exists to avoid.
        //
        // The assistant entry the model was streaming when it made that call
        // is part of the same exchange and is kept with it. Retaining the call
        // but not its host reads to the model as a tool call that appeared out
        // of nowhere, with no preamble to say what it was doing — and the
        // request assembler manufactures an *empty* assistant to carry the call
        // in that case, so the model's own words are dropped while the shape
        // looks fine. An interrupted response is still an unfinished sentence,
        // but the alternative is sending the model a call with no context for
        // it, which is the failure this exclusion exists to prevent.
        //
        // Everything else streaming is still a partial attempt and is taken
        // out: reasoning, and prose from an attempt that made no call at all.
        let answered = self.tool_calls_answered_in_context();
        let carriers = self.entries_hosting_retained_calls(&answered);
        indices.retain(|index| {
            let entry = self.core.history_work.history.get(*index);
            !entry.is_some_and(|entry| answered.contains(&entry.id) || carriers.contains(&entry.id))
        });
        let excluded = self.edit_history().force_exclude_at_indices(&indices);
        if !excluded.is_empty() {
            self.update_view(|v| v.shown_ignored_blocks.extend(excluded.iter().cloned()));
        }
        self.core.ephemeral.clear_streaming_indices();
        excluded
    }

    /// The assistant entries whose adjacent tool calls are in `retained`.
    ///
    /// Walks back from each retained call to the nearest preceding assistant
    /// entry, which is the one it renders into: the request assembler attaches
    /// a call to the most recent assistant message, and synthesizes an empty
    /// one when there is none. Scanning backwards and stopping at the first
    /// assistant mirrors that exactly, so what is kept here is the same entry
    /// that would have carried the call.
    fn entries_hosting_retained_calls(
        &self,
        retained: &HashSet<ChatEntryId>,
    ) -> HashSet<ChatEntryId> {
        let history = &self.core.history_work.history;
        let mut hosts = HashSet::new();
        for (index, entry) in history.iter().enumerate() {
            let ChatEntryKind::ToolCall { .. } = &entry.kind else {
                continue;
            };
            if !retained.contains(&entry.id) {
                continue;
            }
            let host = history
                .get(..index)
                .unwrap_or_default()
                .iter()
                .rev()
                .find(|candidate| matches!(candidate.kind, ChatEntryKind::Assistant(_)));
            if let Some(host) = host {
                hosts.insert(host.id.clone());
            }
        }
        hosts
    }

    /// The ids of tool calls already answered by a result that is itself in
    /// context.
    ///
    /// The pairing is what makes a call a finished exchange, so it is the
    /// test: a call with an answered result belongs in the model's context
    /// even when the attempt that produced it was abandoned mid-stream.
    fn tool_calls_answered_in_context(&self) -> HashSet<ChatEntryId> {
        let history = &self.core.history_work.history;
        let answered: HashSet<&str> = history
            .iter()
            .filter(|entry| entry.is_in_context())
            .filter_map(|entry| match &entry.kind {
                ChatEntryKind::ToolResult { id, .. } => Some(id.as_str()),
                _ => None,
            })
            .collect();
        history
            .iter()
            .filter(|entry| match &entry.kind {
                ChatEntryKind::ToolCall { id, .. } => answered.contains(id.as_str()),
                _ => false,
            })
            .map(|entry| entry.id.clone())
            .collect()
    }

    /// Every history entry index currently tracked by the streaming phase.
    fn collect_streaming_history_indices(&self) -> Vec<usize> {
        let mut indices = Vec::new();
        if let Some(i) = self.core.ephemeral.streaming_entry_index {
            indices.push(i);
        }
        if let Some(i) = self.core.ephemeral.streaming_thinking_entry_index {
            indices.push(i);
        }
        indices.extend(
            self.core
                .ephemeral
                .streaming_tool_call_indices
                .values()
                .copied(),
        );
        indices.extend(
            self.core
                .ephemeral
                .streaming_tool_result_indices
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
        let queue = self.message_queue_mut().drain();
        steering
            .into_iter()
            .chain(queue.into_iter().filter_map(|item| match item {
                jinn_turn_dispatch_msg::QueueItem::UserMessage(entry) => match &entry.kind {
                    ChatEntryKind::User { display, .. } => Some(display.clone()),
                    _ => None,
                },
                jinn_turn_dispatch_msg::QueueItem::ToolContinuation => None,
            }))
            .collect()
    }

    /// Returns the current session lifecycle phase.
    ///
    /// A display projection: written by the turn ledger's admission and
    /// settlement paths. Admission never reads it — the ledger decides.
    #[must_use]
    pub fn phase(&self) -> PhaseView {
        self.core.ephemeral.phase
    }

    /// Create a placeholder `ToolCall` entry and record its history index.
    ///
    /// Called when `ToolUseStarted` arrives - the tool name is known but arguments
    /// are still streaming in.
    ///
    /// Registration happens in whichever busy phase is current, not only in
    /// `Streaming`: a model that opens by calling a tool emits no text token, so
    /// it never reaches `Streaming` and its entire argument stream would
    /// otherwise be discarded.
    pub fn begin_tool_call(
        &mut self,
        index: usize,
        id: &ToolCallId,
        name: &str,
        dispatched_at: jiff::Timestamp,
    ) {
        self.ensure_assistant_entry(dispatched_at);
        self.core.identity.last_provider_activity_at = Timestamp::now();
        // Bail out *before* pushing the entry. A registration that cannot
        // succeed must not leave an orphaned entry behind: it would render as a
        // bare tool name for the rest of the stream, and every later delta
        // addressed to it would be refused.
        if self.core.ephemeral.phase == PhaseView::Idle {
            tracing::warn!(
                index,
                "begin_tool_call called with no turn in flight - ignoring"
            );
            return;
        }
        let mut entry = ChatEntry::tool_call(id.clone(), name, "");
        entry.timing = EntryTiming::streamed(dispatched_at);
        entry.timing.set_first_token();
        stamp_entry_with_generation_model(&mut entry, self.stream_request_model().as_deref());
        let history_index = self.push_entry(entry);
        // The tracking map is turn-scoped and always present, so a
        // registration made here survives every later phase swap — a prose
        // token arriving mid-arguments no longer discards it.
        self.core
            .ephemeral
            .streaming_tool_call_indices
            .insert(index, history_index);
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
            .streaming_tool_call_indices
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
            self.core.identity.last_provider_activity_at = Timestamp::now();
        }
        Ok(())
    }

    /// Overwrite a tool call entry with the final complete arguments.
    ///
    /// Searches recent history for a `ToolCall` entry matching the given ID.
    /// If not found (shouldn't happen in normal flow), pushes a new entry.
    /// Preserves any existing `child_session` link on the entry — only the
    /// streamed-partial fields are finalized here.
    pub fn finalize_tool_call(&mut self, id: &ToolCallId, name: &str, arguments: &str) {
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
                        id: id.clone(),
                        name: name.to_owned(),
                        arguments: arguments.to_owned(),
                        child_session,
                    };
                    entry.timing.finish();
                },
            )
            .is_some();
        if finalized {
            self.core.identity.last_provider_activity_at = Timestamp::now();
        } else {
            // If not found (shouldn't happen), push a new entry.
            self.push_entry(ChatEntry::tool_call(id.clone(), name, arguments));
        }
    }

    /// Create a pending ToolResult entry when a streaming tool starts executing.
    ///
    /// Creates the entry with `ToolResultStatus::Pending` and empty content,
    /// then tracks its history index for later content appends.
    ///
    /// Accepted in either busy phase. A tool result reaches the session in
    /// `Sending` for the ordinary case — the stream ended in `ToolUse`, the
    /// batch ran, and the tools report back while the next dispatch is being
    /// prepared — and in `Streaming` when a batch overlaps a live stream.
    /// Gating on `Streaming` alone dropped every result, and the finalized
    /// result was then pushed detached at the end of history. `Idle` is still
    /// refused so a canceled session does not accumulate orphan entries.
    pub fn begin_tool_result(
        &mut self,
        tool_call_id: &ToolCallId,
        name: &str,
        dispatched_at: jiff::Timestamp,
    ) {
        // No phase guard here: admission is the turn ledger's decision, and
        // this method is only reached through an admitted path. A phase
        // snapshot here would re-create the race the ledger exists to
        // remove (a result created in `Streaming` was once dropped by a
        // `Sending`-only finalizer — bug #21).
        let mut entry =
            ChatEntry::tool_result(tool_call_id.clone(), name, "", ToolResultStatus::Pending);
        entry.timing = EntryTiming::streamed(dispatched_at);
        entry.timing.set_first_token();
        let history_index = self.push_entry(entry);

        // Re-acquire the tracking map after push_entry releases &mut self.
        self.core
            .ephemeral
            .streaming_tool_result_indices
            .insert(tool_call_id.clone(), history_index);
    }

    /// Append incremental output to a pending ToolResult entry.
    ///
    /// Reads the pending entry from whichever busy phase is holding it.
    ///
    /// # Panics
    ///
    /// Panics if no pending entry exists for the given `tool_call_id`.
    pub fn append_tool_result_output(
        &mut self,
        tool_call_id: &ToolCallId,
        output: &str,
        kind: jinn_tools_msg::ToolOutputKind,
    ) {
        let Some(&history_index) = self
            .core
            .ephemeral
            .streaming_tool_result_indices
            .get(tool_call_id)
        else {
            return;
        };
        self.core.identity.last_history_activity_at = jiff::Timestamp::now();
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
    ///
    /// Finalizes an existing ToolResult entry only while it is still
    /// `Pending`, returning whether one was found and resolved.
    ///
    /// The synthesized-cancellation path: a cancel's completion must resolve
    /// a call that never finished but must never overwrite a real result
    /// that landed first — real output wins over the drain's bookkeeping.
    fn finalize_pending_tool_result(
        &mut self,
        tool_call_id: &ToolCallId,
        content: &str,
        status: ToolResultStatus,
    ) -> bool {
        // Untracked pending entry: scan history for the last pending result
        // for this call.
        let index = self
            .core
            .ephemeral
            .streaming_tool_result_indices
            .remove(tool_call_id)
            .or_else(|| {
                self.history().iter().rposition(|entry| {
                    matches!(
                        &entry.kind,
                        ChatEntryKind::ToolResult { id, status, .. }
                            if id == tool_call_id && *status == ToolResultStatus::Pending
                    )
                })
            });
        let Some(index) = index else {
            return false;
        };
        self.edit_history()
            .with_entry_at_mut(index, |entry| {
                if let ChatEntryKind::ToolResult {
                    content: entry_content,
                    status: entry_status,
                    is_alert,
                    ..
                } = &mut entry.kind
                {
                    // Pending only: a real completion that landed between the
                    // snapshot and this write already resolved the entry, and
                    // the synthesis must not clobber it.
                    if *entry_status == ToolResultStatus::Pending {
                        content.clone_into(entry_content);
                        *entry_status = status;
                        *is_alert = false;
                        entry.timing.finish();
                        true
                    } else {
                        false
                    }
                } else {
                    false
                }
            })
            .unwrap_or(false)
    }

    /// Finalizes an existing pending ToolResult entry (whichever busy phase
    /// tracks it, then a history scan), returning whether one was found.
    ///
    /// In-place finalization: never reorders entries or splits a tool loop.
    fn finalize_existing_tool_result(
        &mut self,
        tool_call_id: &ToolCallId,
        content: &str,
        status: ToolResultStatus,
        full_content: Option<String>,
        truncation: Option<jinn_core_types::tool_types::TruncationMeta>,
        pin_position: Option<PinPosition>,
        attachments: Vec<jinn_core_types::attachment::Attachment>,
    ) -> bool {
        let tracked_index = self
            .core
            .ephemeral
            .streaming_tool_result_indices
            .remove(tool_call_id);

        let apply = |entry: &mut ChatEntry| {
            if let ChatEntryKind::ToolResult {
                content: entry_content,
                status: entry_status,
                full_content: entry_full_content,
                truncation: entry_truncation,
                pin_position: entry_kind_pin,
                is_alert,
                attachments: entry_attachments,
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
                *entry_attachments = attachments;
                // Entry-level pin mirrors the kind-level pin so assembly,
                // compaction, and UI consumers read a single field.
                entry.pin_position = pin_position;
                entry.timing.finish();
            }
        };

        match tracked_index {
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

    pub fn finalize_tool_result(
        &mut self,
        tool_call_id: &ToolCallId,
        name: &str,
        content: &str,
        success: bool,
        full_content: Option<String>,
        truncation: Option<jinn_core_types::tool_types::TruncationMeta>,
        pin_position: Option<PinPosition>,
    ) {
        self.finalize_tool_result_with_attachments(
            tool_call_id,
            name,
            content,
            success,
            full_content,
            truncation,
            pin_position,
            Vec::new(),
            None,
        );
    }

    /// [`Self::finalize_tool_result`] for results that produced non-text
    /// attachments (e.g. an image returned by `read_image`).
    ///
    /// `synthesized` is the cancel drain's mark: a synthesized completion
    /// finalizes only an entry that is still `Pending` — a real result that
    /// also landed already finalized it, and the synthesis must not overwrite
    /// real output — and never pushes a new entry. `None` means an ordinary
    /// completion, which keeps the historical push-on-miss behavior.
    #[expect(
        clippy::too_many_arguments,
        reason = "mirrors finalize_tool_result; struct-builder would ripple to all tool actors"
    )]
    pub fn finalize_tool_result_with_attachments(
        &mut self,
        tool_call_id: &ToolCallId,
        name: &str,
        content: &str,
        success: bool,
        full_content: Option<String>,
        truncation: Option<jinn_core_types::tool_types::TruncationMeta>,
        pin_position: Option<PinPosition>,
        attachments: Vec<jinn_core_types::attachment::Attachment>,
        synthesized: Option<bool>,
    ) {
        let status = if success {
            ToolResultStatus::Success
        } else {
            ToolResultStatus::Failure
        };
        let pin_position_result = pin_position;

        let finalized = if synthesized == Some(true) {
            // A synthesized cancellation resolves a call whose entry is still
            // pending; a completed one already carries the real result and is
            // left alone.
            self.finalize_pending_tool_result(tool_call_id, content, ToolResultStatus::Failure)
        } else {
            self.finalize_existing_tool_result(
                tool_call_id,
                content,
                status,
                full_content.clone(),
                truncation.clone(),
                pin_position,
                attachments.clone(),
            )
        };

        if !finalized && synthesized != Some(true) {
            // No existing entry found - push a new one.
            let mut entry = if let Some(meta) = truncation {
                let full = full_content.unwrap_or_default();
                ChatEntry::tool_result_truncated(
                    tool_call_id.clone(),
                    name,
                    content.to_owned(),
                    full,
                    status,
                    meta,
                )
            } else {
                ChatEntry::tool_result_with_attachments(
                    tool_call_id.clone(),
                    name,
                    content,
                    status,
                    attachments,
                )
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
    .core.history_work
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

    /// The turn dispatch queue.
    ///
    /// The queue is a [`TurnQueue`] and this hands back that type rather than
    /// unwrapping it into a bare `VecDeque`: the six methods it used to proxy
    /// (`queue`, `queue_len`, `enqueue`, `enqueue_front`, `dequeue`,
    /// `drain_queue`) were one forwarding call each, so every caller was one
    /// step further from the type that owns the behavior. Callers reach the
    /// operations they need directly on the queue instead.
    ///
    #[must_use]
    pub fn message_queue(&self) -> &jinn_turn_dispatch_msg::TurnQueue {
        &self.core.ephemeral.message_queue
    }

    /// Mutable access to the turn dispatch queue.
    ///
    /// For the operations that consume the queue rather than inspect it —
    /// `drain` and `pop` — which cannot be reached through a shared borrow.
    /// The turn-dispatch actor still pops through [`Self::dequeue`]; this
    /// exists for the paths that take the whole queue at once, such as a
    /// cancel draining everything queued behind the turn it is aborting.
    pub fn message_queue_mut(&mut self) -> &mut jinn_turn_dispatch_msg::TurnQueue {
        &mut self.core.ephemeral.message_queue
    }

    /// Number of items waiting in the queue.
    #[must_use]
    pub fn queue_len(&self) -> usize {
        self.core.ephemeral.message_queue.len()
    }

    /// Push an item onto the back of the queue.
    pub fn enqueue(&mut self, item: jinn_turn_dispatch_msg::QueueItem) {
        self.core.ephemeral.message_queue.enqueue(item);
    }

    /// Push an item onto the front of the queue (for priority items).
    pub fn enqueue_front(&mut self, item: jinn_turn_dispatch_msg::QueueItem) {
        self.core.ephemeral.message_queue.enqueue_front(item);
    }

    /// Pop the front item from the queue, if any.
    ///
    /// The turn-dispatch slice's queue actor is the production caller; the
    /// queue lives on the session, so the pop must be reachable there.
    pub fn dequeue(&mut self) -> Option<jinn_turn_dispatch_msg::QueueItem> {
        self.core.ephemeral.message_queue.pop()
    }

    /// Read-only access to the session profile.
    pub fn profile(&self) -> &SessionProfile {
        &self.core.integrations.profile
    }

    /// Mutable access to the session profile.
    pub fn profile_mut(&mut self) -> &mut SessionProfile {
        &mut self.core.integrations.profile
    }

    /// Set the model selection for this session.
    ///
    /// No endpoint bookkeeping is needed here: a routing pin is a per-model
    /// default in `providers.toml`, so switching models switches which pin
    /// applies by construction rather than by clearing a session field.
    pub fn set_model(&mut self, model: ModelSelection) {
        self.core.integrations.profile.model = model;
    }

    /// Whether this session may use the tool called `tool_name`.
    ///
    /// One predicate for the tool gate, called by both the prompt assembler
    /// and the tool dispatcher. Deciding at separate call sites is what let
    /// the picker list tools the prompt hid, and — worse — what let a tool
    /// absent from the prompt run anyway when the model named it directly.
    ///
    /// This gate is independent of the provider gate (`ToolDefinition::
    /// available_for_provider`), which needs the provider name and stays
    /// where it is. A bot's filter composes as filter ∧ provider ∧
    /// the bot-only gate.
    #[must_use]
    pub fn is_tool_enabled(&self, tool_name: &str) -> bool {
        permits_or_inherits(
            self.core.integrations.profile.tool_filter.as_ref(),
            tool_name,
        )
    }

    /// Read-only access to this session's tool filter.
    ///
    /// `None` means the session has no tool filter and inherits whatever it
    /// would have had. Callers that only need to answer a gate read
    /// [`Self::is_tool_enabled`] instead; this one is for the callers that
    /// seed a picker or hand the filter on.
    #[must_use]
    pub fn tool_filter(&self) -> Option<&NameFilter> {
        self.core.integrations.profile.tool_filter.as_ref()
    }

    /// Replace the tool filter for this session.
    ///
    /// `None` releases it, sending the session back to inheriting. Used by
    /// the tool picker to commit toggle state and by the bot panel to
    /// thaw a frozen set.
    pub fn set_tool_filter(&mut self, filter: Option<NameFilter>) {
        self.core.integrations.profile.tool_filter = filter;
    }

    /// Read-only access to this session's enabled MCP server names.
    ///
    /// Opt-in model: only servers in this set are active for the session.
    #[must_use]
    pub fn enabled_mcp_servers(&self) -> &std::collections::BTreeSet<String> {
        &self.core.integrations.enabled_mcp_servers
    }

    /// Returns `true` if the named MCP server is enabled for this session.
    #[must_use]
    pub fn is_mcp_server_enabled(&self, server: &str) -> bool {
        self.core.integrations.enabled_mcp_servers.contains(server)
    }

    /// Enables an MCP server for this session.
    ///
    /// Returns `true` if the server was not previously enabled (i.e. this call
    /// changed state).
    pub fn enable_mcp_server(&mut self, server: &str) -> bool {
        self.core
            .integrations
            .enabled_mcp_servers
            .insert(server.to_owned())
    }

    /// Disables an MCP server for this session.
    ///
    /// Returns `true` if the server was previously enabled (i.e. this call
    /// changed state).
    pub fn disable_mcp_server(&mut self, server: &str) -> bool {
        self.core.integrations.enabled_mcp_servers.remove(server)
    }

    /// Replaces the entire enabled MCP server set for this session.
    ///
    /// Used by the MCP picker to commit toggle state.
    pub fn set_enabled_mcp_servers(&mut self, servers: std::collections::BTreeSet<String>) {
        self.core.integrations.enabled_mcp_servers = servers;
    }

    /// Whether this session may load the skill called `skill_name`.
    ///
    /// The skill counterpart of [`Self::is_tool_enabled`], sharing its one
    /// predicate so the prompt assembler and the `skill` tool's refusal
    /// path cannot disagree about what the session can load.
    #[must_use]
    pub fn is_skill_enabled(&self, skill_name: &str) -> bool {
        permits_or_inherits(
            self.core.integrations.profile.skill_filter.as_ref(),
            skill_name,
        )
    }

    /// Read-only access to this session's skill filter.
    ///
    /// `None` means the session has no skill filter and inherits whatever it
    /// would have had; see [`Self::tool_filter`].
    #[must_use]
    pub fn skill_filter(&self) -> Option<&NameFilter> {
        self.core.integrations.profile.skill_filter.as_ref()
    }

    /// Replace the skill filter for this session.
    ///
    /// `None` releases it, sending the session back to inheriting. Used by
    /// the skill picker to commit toggle state and by the bot panel to
    /// thaw a frozen set.
    pub fn set_skill_filter(&mut self, filter: Option<NameFilter>) {
        self.core.integrations.profile.skill_filter = filter;
    }

    /// Compute the set of skill names that are currently loaded in this session.
    ///
    /// A skill is considered loaded if its body is present in history as a pinned
    /// ToolResult from the `skill` tool whose content begins with `<skill name="X"`.
    pub fn loaded_skills(&self) -> HashSet<String> {
        use ChatEntryKind;
        use parse_loaded_skill_name;

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
        &self.core.integrations.profile.model
    }
    pub fn model(&self) -> &ModelSelection {
        &self.core.integrations.profile.model
    }

    /// Mark the session as preparing its next request (`Working`).
    ///
    /// The display projection of the tool-use edge: the tool loop has the
    /// stream retired and the calls executing, and the continuation dispatch
    /// re-enters `Streaming` through the ledger's mint.
    pub fn begin_sending(&mut self) {
        self.core.ephemeral.phase = PhaseView::Working;
        self.core.identity.last_history_activity_at = Timestamp::now();
    }

    /// Transition to Working phase (a background operation started).
    ///
    /// Increment the busy counter. Called when a background operation starts.
    /// The count is ephemeral (not persisted).
    /// The current scroll offset (lines to skip from top).
    ///
    /// Returns `None` when auto-scrolled to the bottom, or `Some(n)` when
    /// the user has manually scrolled to a specific offset.
    pub fn scroll_offset(&self) -> Option<u32> {
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
    pub fn set_scroll_offset(&mut self, offset: Option<u32>) {
        self.update_view(|v| v.scroll_offset = offset);
    }

    pub fn scroll_up(&mut self, amount: u16) {
        self.update_view(|v| {
            let current = v
                .scroll_offset
                .unwrap_or(v.last_max_offset.load(Ordering::Relaxed));
            v.scroll_offset = Some(current.saturating_sub(u32::from(amount)));
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
            let next = current.saturating_add(u32::from(amount));
            if next >= v.last_max_offset.load(Ordering::Relaxed) {
                v.scroll_offset = None;
            } else {
                v.scroll_offset = Some(next);
            }
        });
    }

    /// Scroll to the very top of the conversation.
    pub fn scroll_to_top(&mut self) {
        self.update_view(|v| v.scroll_offset = Some(0));
    }

    /// Scroll to the very bottom of the conversation (auto-scroll).
    ///
    /// `None` is the bottom sentinel, not an unset scroll: a session that has
    /// never been scrolled and one scrolled to its last line render the same,
    /// and both follow the streaming entry. This absorbed a second method that
    /// did exactly this and was called the same thing.
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
    pub fn set_last_max_offset(&self, max_offset: u32) {
        self.update_view(|v| v.last_max_offset.store(max_offset, Ordering::Relaxed));
    }

    /// The maximum scroll offset recorded by the last render.
    ///
    /// Meaningful only after the chat-log render pipeline has run for the
    /// current frame; it is the bottom of the document in wrapped lines.
    #[must_use]
    pub fn rendered_max_offset(&self) -> u32 {
        self.with_view(|v| v.last_max_offset.load(Ordering::Relaxed), || 0)
    }

    /// Returns the screen-space Y coordinate of the top of the currently-selected
    /// chat entry within the chat-log area, or `None` if no entry is selected or
    /// the render-pipeline cache is empty.
    ///
    /// The returned Y is in terminal (absolute) coordinates: it already incorporates
    /// `chat_log_area_y` and `blank_count`. Callers can pass it directly as the
    /// `entry_top_y` argument to
    /// the chat-log audit popup geometry helper.
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

                // absolute screen Y; clamped to chat-log area top. A screen row
                // is u16, so narrow from the u32 line math.
                Some(
                    chat_log_area_y
                        .saturating_add(u16::try_from(viewport_offset).unwrap_or(u16::MAX)),
                )
            },
            || None,
        )
    }

    /// Store the rendered scroll offset (actual viewport position after clamping
    /// and scroll-to-selected adjustment). Called by the render pipeline each frame.
    pub fn set_rendered_scroll_offset(&self, offset: u32) {
        self.update_view(|v| v.rendered_scroll_offset.store(offset, Ordering::Relaxed));
    }

    /// Store per-entry wrapped line ranges computed by the renderer.
    ///
    /// `entry_line_ranges[i] = (start_wrapped_line, end_wrapped_line)` in the
    /// wrapped coordinate space. Called each frame by the chat log renderer.
    pub fn set_entry_line_ranges(&self, ranges: Vec<(u32, u32)>) {
        self.update_view(|v| *v.entry_line_ranges.write() = ranges);
    }

    /// Store the viewport height (render area height) from the renderer.
    pub fn set_viewport_height(&self, height: u16) {
        self.update_view(|v| {
            v.viewport_height
                .store(u32::from(height), Ordering::Relaxed);
        });
    }

    /// Read the cached viewport height.
    pub fn viewport_height_value(&self) -> u16 {
        self.with_view(
            |v| u16::try_from(v.viewport_height.load(Ordering::Relaxed)).unwrap_or(u16::MAX),
            || 0,
        )
    }

    /// Store the blank line count prepended for bottom-alignment.
    pub fn set_blank_count(&self, count: u32) {
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
                        .history_work
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
                        .history_work
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
                        .history_work
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
                    .history_work
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
        self.core.history_work.history.replace_all(entries);
        let new_cursor = self.core.history_work.history.last().map(|e| e.id.clone());
        self.update_view(|v| v.selected_cursor_id = new_cursor);
        self.scroll_to_bottom();
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
        let Some(entry) = self.core.history_work.history.iter().find(|e| e.id == *id) else {
            return;
        };
        // Captured before pinning: a pin makes the entry in-context, but the
        // propagation below only applies when the entry was ignored.
        let was_ignored = !entry.is_in_context();
        let index = self
            .core
            .history_work
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
                .history_work
                .history
                .get(block_start - 1)
                .is_some_and(|e| !e.is_in_context())
            && self
                .core
                .history_work
                .history
                .get(block_start - 1)
                .is_some_and(|e| e.pin_position.is_none())
        {
            block_start -= 1;
        }

        let Some(block_entry) = self.core.history_work.history.get(block_start) else {
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
        if forward_start >= self.core.history_work.history.len() {
            return; // No entries after the pin.
        }

        let Some(forward_entry) = self.core.history_work.history.get(forward_start) else {
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
        self.core
            .history_work
            .history
            .iter()
            .filter(|e| e.is_pinned())
            .collect()
    }

    /// Projects the pinned entries into pins-sidebar display rows.
    ///
    /// A pinned tool loop is pinned as a whole (provider APIs require intact
    /// call/result structure), but it displays as one row per completed
    /// `ToolResult` — plus one `⚒` row per call with no completed result in
    /// the loop — while every non-loop pinned entry keeps its own row.
    /// Rows sort TOP → RELATIVE → BOTTOM, stable by history order within
    /// each group, matching the `pin_sort_key` order of sidebar consumers.
    ///
    /// This is a display projection only: pin state itself is unchanged, so
    /// unpinning a row's entry still clears the whole loop's pins.
    #[must_use]
    pub fn pinned_display_rows(&self) -> Vec<PinDisplayRow> {
        let mut rows = display_rows(self.history());
        // Stable sort by display position: TOP, then RELATIVE/None, then
        // BOTTOM, preserving history order within each group.
        rows.sort_by_key(|row| pin_display_sort_key(row.position));
        rows
    }

    /// The number of rows the pins sidebar displays for this session.
    ///
    /// Sidebar consumers must count display rows, not pinned entries: a
    /// pinned tool loop contributes its result rows, not its member count.
    #[must_use]
    pub fn pinned_display_count(&self) -> usize {
        self.pinned_display_rows().len()
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
                    .history_work
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
                .history_work
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
                    .history_work
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
                .history_work
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
                self.core
                    .history_work
                    .history
                    .get(index)
                    .map(|e| e.id.clone())
            } else {
                items.get(index).and_then(|item| {
                    entry_id_from_visual_item(item, &self.core.history_work.history)
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
        if self.core.history_work.history.is_empty() {
            return;
        }
        let max = self.core.history_work.history.len() - 1;
        let start = self
            .selected_entry_index()
            .map_or(0, |i| i.saturating_add(1).min(max));
        let mut idx = start;
        while idx < max
            && self
                .core
                .history_work
                .history
                .get(idx)
                .is_none_or(ChatEntry::is_empty_assistant)
        {
            idx = idx.saturating_add(1);
        }
        if self
            .core
            .history_work
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
        if self.core.history_work.history.is_empty() {
            return;
        }
        let start = self.selected_entry_index().map_or(
            self.core.history_work.history.len().saturating_sub(1),
            |i| i.saturating_sub(1),
        );
        let mut idx = start;
        while idx > 0
            && self
                .core
                .history_work
                .history
                .get(idx)
                .is_none_or(ChatEntry::is_empty_assistant)
        {
            idx = idx.saturating_sub(1);
        }
        if self
            .core
            .history_work
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
            return self
                .core
                .history_work
                .history
                .iter()
                .position(|e| e.id == cursor_id);
        }
        resolve_entry_id_to_vi_index(&cursor_id, &items, &self.core.history_work.history)
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
            return self.core.history_work.history.get(vi_idx);
        }
        match items.get(vi_idx)? {
            VisualItem::Entry(hist_idx) => self.core.history_work.history.get(*hist_idx),
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
        let Some(idx) = self
            .core
            .history_work
            .history
            .iter()
            .position(|e| e.id == *entry_id)
        else {
            return;
        };
        let Some(entry) = self.core.history_work.history.get(idx) else {
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
                .history_work
                .history
                .get(block_start - 1)
                .is_some_and(|e| !e.is_in_context())
            && self
                .core
                .history_work
                .history
                .get(block_start - 1)
                .is_some_and(|e| e.pin_position.is_none())
        {
            block_start -= 1;
        }
        let Some(block_rep) = self.core.history_work.history.get(block_start) else {
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

    /// Publish the visual items list computed during render, replacing the
    /// stored list only when it differs.
    ///
    /// The renderer recomputes the list every frame, but it changes only when
    /// the history, the ignore sets, or the collapse threshold change. An
    /// unconditional write would allocate and copy a list that is one entry
    /// per history entry on every frame. Returns `true` when the stored list
    /// was replaced.
    pub fn set_visual_items_if_changed(&self, items: &[VisualItem]) -> bool {
        self.update_view_taking(|v| Some(v.set_visual_items_if_changed(items)))
            .unwrap_or(false)
    }

    /// Publish the per-entry wrapped line ranges computed during render,
    /// replacing the stored ranges only when they differ.
    ///
    /// The ranges are one pair per visual item and are recomputed every
    /// frame, but the values only change when the wrapped layout does.
    /// Returns `true` when the stored ranges were replaced.
    pub fn set_entry_line_ranges_if_changed(&self, ranges: &[(u32, u32)]) -> bool {
        self.update_view_taking(|v| Some(v.set_entry_line_ranges_if_changed(ranges)))
            .unwrap_or(false)
    }

    /// How many times the visual items list was actually replaced.
    ///
    /// Counts replacements rather than frames, so a caller can tell that a
    /// frame over unchanged history reused the stored list.
    #[must_use]
    pub fn visual_items_writes(&self) -> u64 {
        self.with_view(
            jinn_chat_log_view_msg::ChatLogViewUi::visual_items_writes,
            || 0,
        )
    }

    /// How many times the per-entry line ranges were actually replaced.
    ///
    /// Counts replacements rather than frames, so a caller can tell that a
    /// frame over unchanged history reused the stored ranges.
    #[must_use]
    pub fn entry_line_ranges_writes(&self) -> u64 {
        self.with_view(
            jinn_chat_log_view_msg::ChatLogViewUi::entry_line_ranges_writes,
            || 0,
        )
    }

    /// Store the content width the renderer just measured at.
    ///
    /// The session load reads this back to measure a freshly loaded history
    /// at the width the next frame will use, so the measurement is not
    /// discarded as stale.
    pub fn set_content_width(&self, width: u16) {
        self.update_view(|v| v.content_width.store(u32::from(width), Ordering::Relaxed));
    }

    /// The content width the last render measured at.
    ///
    /// `0` before the first render, which the renderer also treats as "do
    /// not wrap".
    #[must_use]
    pub fn content_width(&self) -> u16 {
        self.with_view(
            |v| u16::try_from(v.content_width.load(Ordering::Relaxed)).unwrap_or(u16::MAX),
            || 0,
        )
    }

    /// Store how many consecutive tool entries the renderer just collapsed.
    ///
    /// A coverage probe reads this back so the visual items it builds match
    /// the ones the next frame will build; see
    /// [`set_content_width`] for the same arrangement.
    pub fn set_min_collapse_count(&self, count: usize) {
        self.update_view(|v| v.min_collapse_count.store(count as u32, Ordering::Relaxed));
    }

    /// The collapse threshold the last render used, or `0` before the first.
    #[must_use]
    pub fn min_collapse_count(&self) -> Option<usize> {
        self.with_view(
            |v| match v.min_collapse_count.load(Ordering::Relaxed) {
                0 => None,
                n => Some(n as usize),
            },
            || None,
        )
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
        self.with_view(|v| v.visual_items.read().get(idx).cloned(), || None)
    }

    /// Whether the cursor is currently on a collapsed ignored block.
    ///
    /// Reads the one item it needs under the view lock instead of copying the
    /// whole list first. The snapshot exists so navigation can resolve indices
    /// against a stable copy while the renderer publishes underneath — but this
    /// asks a yes/no question about one slot, so there is nothing to stabilize:
    /// copying several thousand visual items to discard all but one bought
    /// nothing that holding the lock for one lookup does not already give.
    #[must_use]
    pub fn is_selected_collapsed_block(&self) -> bool {
        let Some(idx) = self.selected_entry_index() else {
            return false;
        };
        self.with_view(
            |v| {
                v.visual_items
                    .read()
                    .get(idx)
                    .is_some_and(|item| matches!(item, VisualItem::CollapsedIgnoredBlock { .. }))
            },
            || false,
        )
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
            VisualItem::Entry(hist_idx) => Some(*hist_idx),
            VisualItem::CollapsedIgnoredBlock { .. } => None,
        }
    }

    /// Returns `true` if the given entry is a `ToolCall` that is still
    /// actively streaming arguments from the LLM.
    pub fn is_tool_call_streaming(&self, entry_id: &ChatEntryId) -> bool {
        let Some(idx) = self
            .core
            .history_work
            .history
            .iter()
            .position(|e| e.id == *entry_id)
        else {
            return false;
        };
        self.core.ephemeral.is_tool_call_at_history_index(idx)
    }

    /// The ids of every `ToolCall` entry currently streaming arguments from the LLM.
    ///
    /// A single snapshot of the streaming state, so callers that need to test many
    /// entries (such as the chat log's per-frame layout pass) pay one map walk instead
    /// of a history scan per entry. Indices with no corresponding history entry are
    /// skipped, matching [`Self::is_tool_call_streaming`]'s treatment of unknown ids.
    pub fn streaming_tool_call_ids(&self) -> HashSet<ChatEntryId> {
        let history: &[ChatEntry] = &self.core.history_work.history;
        self.core
            .ephemeral
            .streaming_tool_call_indices
            .values()
            .filter_map(|&history_index| history.get(history_index))
            .map(|entry| entry.id.clone())
            .collect()
    }

    /// Returns this session's working directory for tool execution.
    pub fn cwd(&self) -> &MarkedPathBuf<Absolute> {
        &self.core.lifecycle.cwd
    }

    /// When this session last saw provider activity (model responses).
    pub fn last_provider_activity_at(&self) -> &Timestamp {
        &self.core.identity.last_provider_activity_at
    }

    /// When this session last saw history activity (new entries appended).
    pub fn last_history_activity_at(&self) -> &Timestamp {
        &self.core.identity.last_history_activity_at
    }

    /// Sets when this session last saw provider activity (streaming/turn).
    pub fn set_last_provider_activity_at(&mut self, ts: Timestamp) {
        self.core.identity.last_provider_activity_at = ts;
    }

    /// Sets when this session last saw history activity (new entries appended).
    pub fn set_last_history_activity_at(&mut self, ts: Timestamp) {
        self.core.identity.last_history_activity_at = ts;
    }

    /// Sets this session's working directory.
    pub fn set_cwd(&mut self, cwd: MarkedPathBuf<Absolute>) {
        self.core.lifecycle.cwd = cwd;
    }

    /// Returns the project directory this session is associated with, if any.
    pub fn project(&self) -> Option<&MarkedPathBuf<Absolute>> {
        self.core.identity.project.as_ref()
    }

    /// Stamps the session's project association. Callers are the projects UI
    /// flow (at session creation) and subagent spawning (inheriting the
    /// parent's stamp); the stamp never follows later cwd changes.
    pub fn set_project(&mut self, project: Option<MarkedPathBuf<Absolute>>) {
        self.core.identity.project = project;
    }

    /// Sets this session's home directory for resolving `@~/path` references.
    pub fn set_home(&mut self, home: MarkedPathBuf<Absolute>) {
        self.core.lifecycle.home = home;
    }

    /// Read-only access to the token ledger.
    pub fn token_ledger(&self) -> &[TokenRecord] {
        &self.core.history_work.token_ledger
    }

    /// Push a token record onto the ledger.
    ///
    /// Records are immutable once pushed - this is the only way to add them.
    pub fn push_token_record(&mut self, record: TokenRecord) {
        self.core.history_work.token_ledger.push(record);
    }

    /// Read-only access to this session's task list.
    pub fn task_list(&self) -> &jinn_tools_msg::TaskList {
        &self.core.history_work.task_list
    }

    /// Mutable access to this session's task list.
    pub fn task_list_mut(&mut self) -> &mut jinn_tools_msg::TaskList {
        &mut self.core.history_work.task_list
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
            .history_work
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
        if let Some(last) = self.core.history_work.token_ledger.last_mut() {
            last.model_used = Some(model);
        }
    }

    /// The parent session, if this session was forked from another.
    pub fn parent_session(&self) -> &Option<SessionId> {
        &self.core.identity.parent_session
    }

    /// The highest entry ordinal inherited from parent at fork time.
    /// `None` for root sessions.
    pub fn fork_ordinal(&self) -> Option<usize> {
        self.core.identity.fork_ordinal
    }

    /// How this session came into being. Identity, not structure —
    /// see [`SessionOrigin`].
    pub fn origin(&self) -> SessionOrigin {
        self.core.identity.origin
    }

    /// Set the fork ordinal for testing and construction.
    pub fn set_fork_ordinal(&mut self, ordinal: usize) {
        self.core.identity.fork_ordinal = Some(ordinal);
    }

    /// Set the session origin for construction paths that decide the kind
    /// after building the session (forks built from snapshots, test
    /// fixtures). Construction paths that know the kind up front use a
    /// dedicated constructor (`new_child`, `new_bot`) instead.
    pub fn set_origin(&mut self, origin: SessionOrigin) {
        self.core.identity.origin = origin;
    }

    /// Set the parent session.
    pub fn set_parent_session(&mut self, parent: SessionId) {
        self.core.identity.parent_session = Some(parent);
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
    // the skill tool. They are NOT persisted.

    /// Returns the skills discovered for this session's cwd tree.
    pub fn discovered_skills(&self) -> &[Skill] {
        &self.core.ephemeral.discovered_skills
    }

    /// Returns the prompt templates discovered for this session's cwd tree.
    pub fn discovered_prompt_templates(&self) -> &PromptTemplateStore {
        &self.core.ephemeral.discovered_prompt_templates
    }

    /// Returns the context files discovered for this session's cwd tree.
    pub fn discovered_context_files(&self) -> &[jinn_context::ContextFile] {
        &self.core.ephemeral.discovered_context_files
    }

    /// Returns the workflows discovered for this session (launch-global).
    pub fn discovered_workflows(&self) -> &[Workflow] {
        &self.core.ephemeral.discovered_workflows
    }

    /// Replaces the discovered skills set for this session (scan-actor write path).
    pub fn set_discovered_skills(&mut self, skills: Vec<Skill>) {
        self.core.ephemeral.discovered_skills = skills;
    }

    /// Replaces the discovered prompt-template store for this session.
    pub fn set_discovered_prompt_templates(&mut self, store: PromptTemplateStore) {
        self.core.ephemeral.discovered_prompt_templates = store;
    }

    /// Replaces the discovered context files for this session.
    pub fn set_discovered_context_files(&mut self, files: Vec<jinn_context::ContextFile>) {
        self.core.ephemeral.discovered_context_files = files;
    }

    /// Replaces the discovered workflows set for this session (scan-actor write path).
    pub fn set_discovered_workflows(&mut self, workflows: Vec<Workflow>) {
        self.core.ephemeral.discovered_workflows = workflows;
    }

    /// Restore the token ledger from persisted data.
    pub fn restore_token_ledger(&mut self, records: Vec<TokenRecord>) {
        self.core.history_work.token_ledger = records;
    }

    /// Restore the parent session from persisted data.
    pub fn restore_parent_session(&mut self, parent: Option<SessionId>) {
        self.core.identity.parent_session = parent;
    }

    /// Restore the updated_at timestamp from persisted data.
    pub fn restore_updated_at(&mut self, ts: jiff::Timestamp) {
        self.core.identity.updated_at = ts;
    }

    /// Restore the creation timestamp from persisted data.
    pub fn restore_created_at(&mut self, ts: jiff::Timestamp) {
        self.core.identity.created_at = ts;
    }

    /// This session's unique identifier.
    pub fn session_id(&self) -> &SessionId {
        &self.core.identity.session_id
    }

    /// Set the session ID (used when inserting into a HashMap with an external key).
    pub fn set_session_id(&mut self, id: SessionId) {
        self.core.identity.session_id = id;
    }

    /// The session title. `None` until the first user message.
    pub fn title(&self) -> Option<&str> {
        self.core.identity.title.as_deref()
    }

    /// Set the session title.
    pub fn set_title(&mut self, title: String) {
        self.core.identity.title = Some(title);
    }

    /// Mark this session as persistent (`true`) or transient (`false`).
    /// Transient sessions (e.g. one-shots) are never written to the store.
    pub fn set_persist(&mut self, persist: bool) {
        self.core.storage.persist = persist;
    }

    /// When this session was last updated.
    pub fn updated_at(&self) -> &Timestamp {
        &self.core.identity.updated_at
    }

    /// When this session was created. Never changes after construction.
    pub fn created_at(&self) -> &Timestamp {
        &self.core.identity.created_at
    }

    /// Update the timestamp to now.
    pub fn touch(&mut self) {
        self.core.identity.updated_at = Timestamp::now();
    }

    /// Generic blob storage for future subsystems.
    pub fn blobs(&self) -> &HashMap<String, JsonValue> {
        &self.core.integrations.blobs
    }

    /// Mutable access to generic blob storage.
    pub fn blobs_mut(&mut self) -> &mut HashMap<String, JsonValue> {
        &mut self.core.integrations.blobs
    }

    /// The name of the lifecycle that created this session, if any.
    pub fn lifecycle_name(&self) -> Option<&str> {
        self.core.lifecycle.lifecycle_name.as_deref()
    }

    /// Set the lifecycle name.
    pub fn set_lifecycle_name(&mut self, name: Option<String>) {
        self.core.lifecycle.lifecycle_name = name;
    }

    /// The args used during setup (replayed for teardown), as ordered
    /// name-value pairs.
    pub fn lifecycle_args(&self) -> &LifecycleArgs {
        &self.core.lifecycle.lifecycle_args
    }

    /// Set the lifecycle args.
    pub fn set_lifecycle_args(&mut self, args: LifecycleArgs) {
        self.core.lifecycle.lifecycle_args = args;
    }

    /// The workflow-local lifecycle file this session's lifecycle was
    /// resolved from, if any. `None` means jinn.toml.
    pub fn lifecycle_source(&self) -> Option<&str> {
        self.core.lifecycle.lifecycle_source.as_deref()
    }

    /// Set the workflow-local lifecycle file the lifecycle resolved from.
    pub fn set_lifecycle_source(&mut self, source: Option<String>) {
        self.core.lifecycle.lifecycle_source = source;
    }

    /// Returns the session's memory state.
    pub fn session_state(&self) -> SessionState {
        self.core.storage.session_state
    }

    /// Sets the session's memory state.
    pub fn set_session_state(&mut self, state: SessionState) {
        self.core.storage.session_state = state;
    }

    /// Whether this session belongs to the sidebar's Sessions list.
    ///
    /// A listed session is one that is in memory (`Loaded`) and is not a
    /// Library member: the Sessions tree lists live conversations, while a
    /// member has been *moved* to the Library section and is hidden from it.
    /// Every consumer that filters the session map into the Sessions list
    /// must ask this instead of restating the conjunction, so the list, the
    /// memoization key, and the cursor's index space cannot disagree.
    #[must_use]
    pub fn is_listed(&self) -> bool {
        self.core.storage.session_state == SessionState::Loaded && !self.core.storage.in_library
    }

    /// Whether this session belongs to the sidebar's Library section.
    pub fn in_library(&self) -> bool {
        self.core.storage.in_library
    }

    /// Sets this session's Library membership.
    pub fn set_in_library(&mut self, in_library: bool) {
        self.core.storage.in_library = in_library;
    }

    /// Returns the lifecycle script state.
    pub fn lifecycle_script_state(&self) -> LifecycleScriptState {
        self.core.lifecycle.lifecycle_script_state
    }

    /// Advances lifecycle state after successful setup: `NothingRan → SetupRan`.
    pub fn advance_lifecycle_after_setup(&mut self) {
        self.core
            .lifecycle
            .lifecycle_script_state
            .advance_after_setup();
    }

    /// Advances lifecycle state after successful teardown: `SetupRan → TeardownRan`.
    pub fn advance_lifecycle_after_teardown(&mut self) {
        self.core
            .lifecycle
            .lifecycle_script_state
            .advance_after_teardown();
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
    ///
    /// The excluded entries are also registered as an expanded ignored block,
    /// for the same reason [`Self::reset_streaming_entries_for_retry`] does it:
    /// an interrupted attempt is typically three entries — exactly the collapse
    /// threshold — so exclusion alone would reduce the whole attempt to a single
    /// "N hidden entries" line.
    ///
    /// Returns the ids whose context override changed.
    pub fn force_exclude_dangling_tool_calls(&mut self) -> Vec<ChatEntryId> {
        let excluded = self.edit_history().exclude_incomplete_trailing_loops();
        if !excluded.is_empty() {
            self.update_view(|v| v.shown_ignored_blocks.extend(excluded.iter().cloned()));
        }
        excluded
    }

    /// Disable the tool loop for this session's current turn.
    ///
    pub fn set_tool_loop_disabled(&mut self) {
        self.core.ephemeral.set_tool_loop_disabled();
    }

    /// Take the tool-loop-disabled flag, clearing it.
    ///
    pub fn take_tool_loop_disabled(&mut self) -> bool {
        self.core.ephemeral.take_tool_loop_disabled()
    }

    /// Check whether the tool loop is disabled, without clearing.
    ///
    pub fn is_tool_loop_disabled(&self) -> bool {
        self.core.ephemeral.is_tool_loop_disabled()
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

    /// Queue a batch of mutations for deferred application.
    ///
    /// Empty batches are silently ignored.
    pub fn queue_mutations(&mut self, batch: Vec<HistoryMutation>) {
        if !batch.is_empty() {
            self.core.ephemeral.pending_mutations.push(batch);
        }
    }

    /// Drain all pending mutation batches.
    pub fn drain_pending_mutations(&mut self) -> Vec<Vec<HistoryMutation>> {
        std::mem::take(&mut self.core.ephemeral.pending_mutations)
    }

    /// Apply a batch of mutations. Resolves IDs to current positions.
    ///
    /// Silently skips mutations targeting nonexistent entries.
    /// Processing order within a batch is preserved - earlier mutations
    /// are visible to later ones in the same batch.
    pub fn apply_mutations(&mut self, batch: Vec<HistoryMutation>) -> Vec<ChatEntryId> {
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
        value: ContextOverride,
        source: ChangeSource,
        token_cost: u32,
    ) {
        self.core
            .ephemeral
            .accumulated_overrides
            .push(entry_id, value, source, token_cost);
    }

    /// Arm the current inference-stream generation.
    pub fn arm_stream(&mut self, dispatched_at: Timestamp) {
        self.core.ephemeral.stream_dispatched_at = Some(dispatched_at);
    }

    /// Record the model requested for the current inference-stream generation.
    ///
    /// An empty string (the no-provider sentinel `accum.model_id`) is
    /// normalized to `None` so no entry carries an empty model id. A
    /// dispatch without a receipt always leaves the previous generation's
    /// value in place — overwrite-on-dispatch is the mechanism that keeps
    /// the stamp with its generation.
    pub fn set_stream_request_model(&mut self, model: Option<String>) {
        let normalized = model.filter(|m| !m.is_empty());
        self.core.ephemeral.stream_request_model = normalized;
    }

    /// Return the model requested for the active inference-stream generation.
    #[must_use]
    pub fn stream_request_model(&self) -> Option<String> {
        self.core.ephemeral.stream_request_model.clone()
    }

    /// Drop the requested-model stamp along with the generation it belongs to.
    pub fn clear_stream_request_model(&mut self) {
        self.core.ephemeral.stream_request_model = None;
    }

    /// Return the active inference-stream generation marker.
    #[must_use]
    pub fn stream_dispatched_at(&self) -> Option<Timestamp> {
        self.core.ephemeral.stream_dispatched_at
    }

    /// Consume the active inference-stream generation marker.
    pub fn clear_stream_generation(&mut self) {
        self.core.ephemeral.stream_dispatched_at = None;
    }

    /// Return whether an inference request is currently in flight.
    #[must_use]
    pub fn has_in_flight_stream(&self) -> bool {
        self.core.ephemeral.stream_dispatched_at.is_some()
    }

    /// Return the number of deferred history-mutation batches.
    #[must_use]
    pub fn pending_mutation_count(&self) -> usize {
        self.core.ephemeral.pending_mutations.len()
    }

    /// Return whether history mutations are currently deferred.
    #[must_use]
    pub fn has_pending_mutations(&self) -> bool {
        !self.core.ephemeral.pending_mutations.is_empty()
    }

    /// Apply a context override to an entry by current history index.
    pub fn set_entry_context_override_at(
        &mut self,
        index: usize,
        value: ContextOverride,
        source: &ChangeSource,
    ) -> bool {
        self.edit_history()
            .with_entry_at_mut(index, |entry| {
                entry.apply_context_override(value, source.clone());
            })
            .is_some()
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

/// One row of the pins sidebar: a representative entry plus how to label it.
///
/// The projection from pinned entries to rows lives in
/// [`ChatSessionState::pinned_display_rows`]; this type is only its output.
/// A row's `entry_id` is always a real history entry id, so selection,
/// unpin, and pin-cycle operations address it like any pinned entry, while
/// chunk pin semantics stay unchanged (unpinning still clears the loop).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinDisplayRow {
    /// The id the row's selection, unpin, and pin-cycle operations address:
    /// the id of the representative entry itself.
    pub entry_id: ChatEntryId,
    /// The pin position the row's badge shows, inherited from the entry.
    pub position: PinPosition,
    /// What the row displays.
    pub label: PinRowLabel,
}

/// What a pins-sidebar display row shows.
///
/// The sidebar turns a label into styled text; the prefix is rendered from
/// the variant (`Verbatim` keeps the entry kind's usual prefix, `Result` and
/// `SkillResult` render with no prefix, `Call` renders with the `⚒` prefix).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PinRowLabel {
    /// Every non-tool-loop entry kind, displayed exactly as today under its
    /// existing per-kind prefix.
    Verbatim,
    /// A completed tool result: `✓ name: content` on success,
    /// `✗ name: content` on failure.
    Result {
        /// The tool's name.
        name: String,
        /// The result content.
        content: String,
        /// Whether the result completed successfully (`✗` otherwise).
        success: bool,
    },
    /// A loaded-skill tool result: the skill summary label, no prefix.
    SkillResult {
        /// The result content, parsed into the skill summary label.
        content: String,
    },
    /// A tool call without a completed result in its loop: `name(...)`.
    Call {
        /// The tool's name.
        name: String,
    },
}

/// The display sort weight for a pin position: TOP first, then RELATIVE
/// (or absent), then BOTTOM — the same order the sidebar consumers apply.
fn pin_display_sort_key(position: PinPosition) -> u8 {
    match position {
        PinPosition::Top => 0,
        PinPosition::Relative => 1,
        PinPosition::Bottom => 2,
    }
}

/// Projects a history into display rows in history order (unsorted).
///
/// Standalone pinned entries become one row each. A pinned tool loop
/// contributes one row per completed result, one row per call with no
/// completed result, and nothing for the opener or interstitials. Call rows
/// appear where the call sits; result rows where the result sits — history
/// order, so parallel calls/results interleave naturally.
fn display_rows(history: &[ChatEntry]) -> Vec<PinDisplayRow> {
    let chunks = chunking(history);

    let mut rows = Vec::new();
    for (index, entry) in history.iter().enumerate() {
        let Some(position) = entry.pin_position else {
            continue;
        };
        // Every index below history.len() lies in some chunk, so this can
        // only miss if `chunking` and the enumerate disagree — skip, not panic.
        let Some(row_chunk) = chunk_containing(&chunks, index) else {
            continue;
        };

        if is_standalone(&row_chunk) {
            rows.push(standalone_row(entry, position));
            continue;
        }

        let Some(row) = loop_member_row(entry, &row_chunk, history, position) else {
            continue;
        };
        rows.push(row);
    }
    rows
}

/// Whether a chunk is a single standalone entry rather than a tool loop.
fn is_standalone(chunk: &Chunk) -> bool {
    chunk.range.len() == 1
}

/// The display row for a standalone pinned entry (a single-entry chunk).
///
/// Every non-loop kind keeps its own row under its existing prefix; tool
/// kinds pinned on their own (never produced by normal flow, but legal in
/// restored history) project the same label text a loop member would.
fn standalone_row(entry: &ChatEntry, position: PinPosition) -> PinDisplayRow {
    let label = match &entry.kind {
        ChatEntryKind::ToolResult {
            name,
            content,
            status,
            ..
        } => result_label(content, name, *status),
        ChatEntryKind::ToolCall { name, .. } => call_label(name),
        _ => PinRowLabel::Verbatim,
    };
    PinDisplayRow {
        entry_id: entry.id.clone(),
        position,
        label,
    }
}

/// The display row for one member of a pinned multi-entry tool loop.
///
/// Returns `None` for members that render no row: the assistant opener,
/// interstitial entries (system, actor, thinking, transient, annotation,
/// rule-interrupt guidance), and *pending* results — a pending result is
/// represented by its call's row, so when the result completes the call's
/// ⚒ row becomes the result's ✓ row in place and the row count is stable.
/// A completed result always renders its row; a call renders only when no
/// completed result for its id exists anywhere in the same chunk.
fn loop_member_row(
    entry: &ChatEntry,
    chunk: &Chunk,
    history: &[ChatEntry],
    position: PinPosition,
) -> Option<PinDisplayRow> {
    match &entry.kind {
        ChatEntryKind::ToolResult {
            name,
            content,
            status,
            ..
        } if *status != ToolResultStatus::Pending => Some(PinDisplayRow {
            entry_id: entry.id.clone(),
            position,
            label: result_label(content, name, *status),
        }),
        ChatEntryKind::ToolCall { id, name, .. } => {
            if chunk_has_completed_result_for(history, chunk, id) {
                return None;
            }
            Some(PinDisplayRow {
                entry_id: entry.id.clone(),
                position,
                label: call_label(name),
            })
        }
        // Assistant opener and interstitials never display a row.
        _ => None,
    }
}

/// Whether any completed (non-pending) result for `call_id` sits inside
/// `chunk`.
///
/// Matching is by id within the chunk, never by adjacency, so a loop whose
/// calls and results are separated by interstitials still resolves — and a
/// result outside the chunk can never satisfy a call inside it.
fn chunk_has_completed_result_for(
    history: &[ChatEntry],
    chunk: &Chunk,
    call_id: &ToolCallId,
) -> bool {
    let members = history.get(chunk.range.start..chunk.range.end);
    members.is_some_and(|members| {
        members.iter().any(|member| {
            matches!(
                &member.kind,
                ChatEntryKind::ToolResult { id, status, .. }
                    if id == call_id && *status != ToolResultStatus::Pending
            )
        })
    })
}

/// Assembles the label for a tool result row.
///
/// Loaded skills are pinned as `<skill name="X" ...>` XML; they display the
/// parsed skill summary instead of the raw content. Every other result is
/// `icon name: content`, `✓` for success and `✗` for failure.
fn result_label(content: &str, name: &str, status: ToolResultStatus) -> PinRowLabel {
    if name == "skill" {
        return PinRowLabel::SkillResult {
            content: loaded_skill_summary_label(content),
        };
    }
    PinRowLabel::Result {
        name: name.to_owned(),
        content: content.to_owned(),
        success: status == ToolResultStatus::Success,
    }
}

/// Assembles the label for a pending-call row.
fn call_label(name: &str) -> PinRowLabel {
    PinRowLabel::Call {
        name: name.to_owned(),
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
