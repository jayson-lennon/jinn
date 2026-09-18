//! Conversation data model for the chat log.
//!
//! Each [`ChatEntry`] records a timed message from the user,
//! the system, or an actor.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::feat::session::tool_result_status::ToolResultStatus;
use crate::protocol::SessionId;

// Re-export shim: `ChatEntryId` moved to `jinn-core-types` (a pure value
// type, needed by `jinn-slices` chat-log view vocabulary); the kernel path
// stays stable for consumers.
pub use jinn_core_types::ChatEntryId;

/// Where a pinned entry should appear in the assembled prompt.
///
/// Entries with a pin position are never discarded by prompt assembly strategies
/// (sliding window, token budget, compaction). The position controls *where*
/// they appear in the final assembled prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PinPosition {
    /// Always appear at the very beginning of the assembled prompt.
    Top,
    /// Always appear just before the most recent message.
    Bottom,
    /// Stay at this entry's original position in history.
    Relative,
}

impl std::fmt::Display for PinPosition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Top => write!(f, "TOP"),
            Self::Bottom => write!(f, "BOTTOM"),
            Self::Relative => write!(f, "RELATIVE"),
        }
    }
}

impl From<jinn_core_types::tool_types::ToolResultPinPosition> for PinPosition {
    fn from(pos: jinn_core_types::tool_types::ToolResultPinPosition) -> Self {
        match pos {
            jinn_core_types::tool_types::ToolResultPinPosition::Top => Self::Top,
            jinn_core_types::tool_types::ToolResultPinPosition::Bottom => Self::Bottom,
            jinn_core_types::tool_types::ToolResultPinPosition::Relative => Self::Relative,
        }
    }
}

// Re-export shim: `ContextOverride` moved to `jinn-core-types` (a pure
// value type, needed by `jinn-slices` chat-log view vocabulary); the
// kernel path stays stable for consumers.
pub use jinn_core_types::ContextOverride;

/// Who initiated a change to a [`ChatEntry`]'s `context_override`.
///
/// Recorded on every [`ContextChangeEvent`] so the audit trail can answer
/// "why is this entry currently excluded?".
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChangeSource {
    /// The user (via the `x` key toggle or sweep).
    User,
    /// A background history worker (compaction or one of the auto-prune workers).
    ///
    /// `name` matches the worker's [`HistoryWorker::name`] so adding a new worker
    /// requires zero changes here.
    ///
    /// [`HistoryWorker::name`]: crate::feat::history_worker::worker_trait::HistoryWorker::name
    Worker { name: String },
    /// An internal session-actor sweep that isn't a worker (e.g. dangling-tool-call cleanup).
    Internal { label: String },
}

/// A single recorded change to a [`ChatEntry`]'s `context_override`.
///
/// Held in [`ChatEntry::context_history`]. Append-only; never mutated once recorded.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ContextChangeEvent {
    /// The override value before this change.
    pub from: ContextOverride,
    /// The override value after this change.
    pub to: ContextOverride,
    /// Who initiated the change.
    pub source: ChangeSource,
    /// When the change was applied.
    pub timestamp: jiff::Timestamp,
}

/// Per-`@path` token resolution outcome for a user entry.
///
// Recorded after `@path` resolution runs so the chat render can color each
// token by outcome (green = attached, red = degraded) and so re-expansion
// leaves degraded tokens as literal text. Empty (default) for plain-text
// messages and all non-`User` kinds.
//
// OWNER: session-actor (set once during enqueue resolution).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AttachmentOutcome {
    /// `@path` tokens that attached successfully as images.
    ///
    /// Each entry pairs the literal token body (as the user typed it, e.g.
    /// `img.png`, `~/photo.png`, `/abs/x.heic`) with the absolute path that
    /// was resolved and attached. The literal is the render-time key into the
    /// display text; the absolute path is what actually got attached.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attached: Vec<ResolvedToken>,
    /// `@path` tokens that degraded (missing file or not a recognized image).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub degraded: Vec<ResolvedToken>,
}

/// A resolved `@path` token: the literal text the user typed, paired with the
/// absolute path it resolved to.
///
// Resolution happens exactly once at enqueue; the render layer reads this
// frozen result and never touches the filesystem or the session cwd/home.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ResolvedToken {
    /// The literal token body as it appears in the display text (after the `@`).
    pub raw: String,
    /// The absolute path the token resolved to at enqueue time.
    pub abs: std::path::PathBuf,
}

impl AttachmentOutcome {
    /// Whether both outcome sets are empty (no resolution ran, or plain text).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.attached.is_empty() && self.degraded.is_empty()
    }
}

/// A single entry in the chat history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatEntry {
    /// Unique identifier for this entry.
    pub id: ChatEntryId,
    /// Timing data for this entry.
    pub timing: super::entry_timing::EntryTiming,
    /// What kind of entry this is.
    pub kind: ChatEntryKind,
    /// Whether this entry is pinned to the context, and where.
    ///
    /// Pinned entries are never discarded by prompt assembly strategies.
    /// `None` (default) means the entry is not pinned.
    ///
    /// OWNER: context-actor (individual mutations via PinChatEntry/UnpinChatEntry),
    ///        session-actor (atomic bulk restore during SessionLoadCompleted via restore_history).
    #[serde(default)]
    pub pin_position: Option<PinPosition>,
    /// User-controlled override for whether this entry is included in LLM context.
    ///
    /// Tri-state: `Default` follows the kind-level rule, `ForcedInclude` forces the
    /// entry into context regardless of kind default, `ForcedExclude` forces it out.
    ///
    /// Pin overrides this field: if an entry is both pinned and `ForcedExclude`,
    /// it is still included in prompt assembly.
    ///
    /// OWNER: compaction-actor (sets `ForcedExclude` during compaction),
    ///        user (via `x` key toggle in `toggle_entry_ignored`, or the `r`
    ///        key reset).
    #[serde(default)]
    pub context_override: ContextOverride,

    /// Append-only audit log of every change to this entry's `context_override`.
    ///
    /// Empty for newly-created entries; populated by [`Self::apply_context_override`].
    /// Old persisted entries lacking this field load as an empty Vec via
    /// `#[serde(default)]` - no schema migration is required.
    #[serde(default)]
    pub context_history: Vec<ContextChangeEvent>,

    /// Estimated token count of this entry's content (tiktoken, o200k_base).
    ///
    /// Content-derived and context-independent: it does NOT change when the
    /// entry is excluded from LLM context. `None` means not yet computed; the
    /// token count actor fills it in memory and the next session persist saves
    /// it. Persisted in `entries.token_count` (v25).
    ///
    /// OWNER comment: token-count-actor (fills `None` counts),
    ///        session-actor (bulk DB restore via `restore_history`).
    #[serde(default)]
    pub token_count: Option<u32>,
}

/// The kind of chat entry.
#[derive(Debug, Clone, PartialEq)]
pub enum ChatEntryKind {
    /// A message typed by the user.
    ///
    /// `display` is what the user typed (shown in the UI, used for session titles).
    /// `expanded` is the token-expanded text (sent to the LLM).
    /// When no prompt tokens are present, both fields are identical.
    User {
        display: String,
        expanded: String,
        attachments: Vec<jinn_provider::Attachment>,
        outcome: AttachmentOutcome,
    },
    /// A system-generated message (status updates, etc.).
    System(String),
    /// An error message displayed prominently (e.g., stream cancelled).
    Error(String),
    /// A response from an AI assistant.
    Assistant(String),
    /// A message from an actor, identified by source name.
    Actor {
        /// The name of the actor that produced this entry.
        source: String,
        /// The message text.
        text: String,
    },
    /// Reasoning/thinking content extracted from the LLM response.
    ///
    /// Displayed in the chat log but excluded from context assembly.
    /// Models like DeepSeek-R1 and Qwen3 embed reasoning in `<think>` tags;
    /// the `reasoning-parser` crate extracts it during streaming.
    Thinking(String),
    /// A tool call requested by the LLM.
    ToolCall {
        /// Unique ID assigned by the LLM provider.
        id: String,
        /// The function name.
        name: String,
        /// The JSON arguments string.
        arguments: String,
        /// The child session this call spawned (`task` tool only; `None`
        /// otherwise). Persisted with the entry so the link survives
        /// restarts; serde-defaulted so pre-link sessions round-trip.
        child_session: Option<SessionId>,
    },
    /// The result of executing a tool call.
    ToolResult {
        /// The ID of the tool call this result is for.
        id: String,
        /// The function name.
        name: String,
        /// The output content.
        content: String,
        /// Execution status (pending, success, or failure).
        status: ToolResultStatus,
        /// Full untruncated content, if truncation occurred.
        full_content: Option<String>,
        /// Truncation metadata, if truncation occurred.
        truncation: Option<jinn_core_types::tool_types::TruncationMeta>,
        /// Where this entry should appear in the assembled prompt. `None` (default)
        /// means the entry participates in normal history compaction/trimming.
        pin_position: Option<PinPosition>,
        /// Render-time hint: the pending output was an alert (e.g. a bot
        /// challenge awaiting a human solve). Cleared on finalize. The
        /// custom serde impls write it as `"is_alert"` and default it to
        /// `false` for old persisted sessions.
        is_alert: bool,
    },

    /// A transient UI-only message (not sent to the LLM).
    ///
    /// Used for welcome messages, status notifications, and other ephemeral
    /// user-facing hints. Excluded from prompt assembly, token estimation,
    /// LLM context, and session persistence. Cannot be pinned.
    ///
    /// Contains markdown text rendered at display time by the chat log renderer.
    /// Supports markdown tables, inline formatting, and proper reflow on resize.
    Transient(String),
    /// A compaction summary entry created by the compaction actor.
    ///
    /// Contains a structured summary of previous conversation history
    /// generated by an LLM. Acts as a delimiter in the chat history -
    /// entries before a compaction that are not pinned or system messages
    /// are marked `ignored` and skipped during prompt assembly.
    ///
    /// Multiple compactions are carried forward as-is; each new compaction
    /// only summarizes entries between the previous compaction and now.
    Compaction {
        /// The LLM-generated structured summary text.
        summary: String,
        /// Estimated tokens in the compacted region before compaction.
        tokens_before: usize,
        /// Estimated tokens in the compaction summary (after compaction).
        tokens_after: usize,
        /// How many entries were compacted (marked as ignored).
        entries_compacted: usize,
        /// Which model was used to generate the summary.
        model_used: String,
    },
    /// Source citations attached to an assistant message (e.g. OpenRouter
    /// `url_citation` annotations).
    ///
    /// Display-only: rendered in the chat log as a grouped source list but
    /// excluded from LLM context assembly (`entries_to_messages`), token
    /// estimation, and compaction serialization. Persisted across session reload
    /// so citations survive a reopen.
    Annotation {
        /// The citations captured for this turn (one entry per search).
        citations: Vec<jinn_provider::UrlCitation>,
    },
}

impl ChatEntry {
    /// Create a new user chat entry with the current timestamp.
    #[must_use]
    pub fn user<T>(text: T) -> Self
    where
        T: Into<String>,
    {
        let t = text.into();
        Self {
            id: ChatEntryId::new(),
            timing: super::entry_timing::EntryTiming::instant_now(),
            kind: ChatEntryKind::User {
                display: t.clone(),
                expanded: t,
                attachments: Vec::new(),
                outcome: AttachmentOutcome::default(),
            },
            pin_position: None,
            context_override: ContextOverride::Default,
            context_history: Vec::new(),
            token_count: None,
        }
    }

    /// Returns the primary text this entry contributes to the assembled prompt,
    /// mirroring the per-kind extraction in the token estimator.
    ///
    /// Used as a fallback for the accumulation gate's token-cost resolver when
    /// an entry isn't in the worker token cache (rare — workers cache before
    /// producing mutations). Returns `None` for kinds with no single primary
    /// slice. Note this does not apply estimator prefixes; it's an approximation
    /// for the rare cache miss.
    #[must_use]
    pub fn prompt_text(&self) -> Option<&str> {
        match &self.kind {
            ChatEntryKind::User { expanded, .. } => Some(expanded.as_str()),
            ChatEntryKind::Assistant(t)
            | ChatEntryKind::System(t)
            | ChatEntryKind::Error(t)
            | ChatEntryKind::Thinking(t)
            | ChatEntryKind::Transient(t)
            | ChatEntryKind::Compaction { summary: t, .. } => Some(t.as_str()),
            ChatEntryKind::Actor { text, .. } => Some(text.as_str()),
            ChatEntryKind::ToolCall { arguments, .. } => Some(arguments.as_str()),
            ChatEntryKind::ToolResult { content, .. } => Some(content.as_str()),
            // Annotations are display-only: no prompt contribution.
            ChatEntryKind::Annotation { .. } => None,
        }
    }

    /// Create a user entry with separate display and expanded text.
    ///
    /// Use when prompt token expansion produces a different expanded text
    /// than what the user typed.
    #[must_use]
    pub fn user_expanded<D, E>(display: D, expanded: E) -> Self
    where
        D: Into<String>,
        E: Into<String>,
    {
        Self {
            id: ChatEntryId::new(),
            timing: super::entry_timing::EntryTiming::instant_now(),
            kind: ChatEntryKind::User {
                display: display.into(),
                expanded: expanded.into(),
                attachments: Vec::new(),
                outcome: AttachmentOutcome::default(),
            },
            pin_position: None,
            context_override: ContextOverride::Default,
            context_history: Vec::new(),
            token_count: None,
        }
    }

    /// Create a new system chat entry with the current timestamp.
    #[must_use]
    pub fn system<T>(text: T) -> Self
    where
        T: Into<String>,
    {
        Self {
            id: ChatEntryId::new(),
            timing: super::entry_timing::EntryTiming::instant_now(),
            kind: ChatEntryKind::System(text.into()),
            pin_position: None,
            context_override: ContextOverride::Default,
            context_history: Vec::new(),
            token_count: None,
        }
    }

    /// Create a new error chat entry with the current timestamp.
    #[must_use]
    pub fn error<T>(text: T) -> Self
    where
        T: Into<String>,
    {
        Self {
            id: ChatEntryId::new(),
            timing: super::entry_timing::EntryTiming::instant_now(),
            kind: ChatEntryKind::Error(text.into()),
            pin_position: None,
            context_override: ContextOverride::Default,
            context_history: Vec::new(),
            token_count: None,
        }
    }

    /// Create a new assistant chat entry with the current timestamp.
    #[must_use]
    pub fn assistant<T>(text: T) -> Self
    where
        T: Into<String>,
    {
        Self {
            id: ChatEntryId::new(),
            timing: super::entry_timing::EntryTiming::instant_now(),
            kind: ChatEntryKind::Assistant(text.into()),
            pin_position: None,
            context_override: ContextOverride::Default,
            context_history: Vec::new(),
            token_count: None,
        }
    }

    /// Create a new actor chat entry with the current timestamp.
    #[must_use]
    pub fn actor<S, T>(source: S, text: T) -> Self
    where
        S: Into<String>,
        T: Into<String>,
    {
        Self {
            id: ChatEntryId::new(),
            timing: super::entry_timing::EntryTiming::instant_now(),
            kind: ChatEntryKind::Actor {
                source: source.into(),
                text: text.into(),
            },
            pin_position: None,
            context_override: ContextOverride::Default,
            context_history: Vec::new(),
            token_count: None,
        }
    }

    /// Create a new thinking entry with the current timestamp.
    #[must_use]
    pub fn thinking<T>(text: T) -> Self
    where
        T: Into<String>,
    {
        Self {
            id: ChatEntryId::new(),
            timing: super::entry_timing::EntryTiming::instant_now(),
            kind: ChatEntryKind::Thinking(text.into()),
            pin_position: None,
            context_override: ContextOverride::Default,
            context_history: Vec::new(),
            token_count: None,
        }
    }

    /// Create a new tool call entry with the current timestamp.
    #[must_use]
    pub fn tool_call<S1, S2, S3>(id: S1, name: S2, arguments: S3) -> Self
    where
        S1: Into<String>,
        S2: Into<String>,
        S3: Into<String>,
    {
        Self {
            id: ChatEntryId::new(),
            timing: super::entry_timing::EntryTiming::instant_now(),
            kind: ChatEntryKind::ToolCall {
                id: id.into(),
                name: name.into(),
                arguments: arguments.into(),
                child_session: None,
            },
            pin_position: None,
            context_override: ContextOverride::Default,
            context_history: Vec::new(),
            token_count: None,
        }
    }

    /// Create a new tool result entry with the current timestamp.
    #[must_use]
    pub fn tool_result<S1, S2, S3>(id: S1, name: S2, content: S3, status: ToolResultStatus) -> Self
    where
        S1: Into<String>,
        S2: Into<String>,
        S3: Into<String>,
    {
        Self {
            id: ChatEntryId::new(),
            timing: super::entry_timing::EntryTiming::instant_now(),
            kind: ChatEntryKind::ToolResult {
                id: id.into(),
                name: name.into(),
                content: content.into(),
                status,
                full_content: None,
                truncation: None,
                is_alert: false,
                pin_position: None,
            },
            pin_position: None,
            context_override: ContextOverride::Default,
            context_history: Vec::new(),
            token_count: None,
        }
    }

    /// Create a new annotation entry holding source citations.
    ///
    /// Annotations are display-only: rendered in the chat log but excluded from
    /// LLM context assembly, token estimation, and compaction.
    #[must_use]
    pub fn annotation(citations: Vec<jinn_provider::UrlCitation>) -> Self {
        Self {
            id: ChatEntryId::new(),
            timing: super::entry_timing::EntryTiming::instant_now(),
            kind: ChatEntryKind::Annotation { citations },
            pin_position: None,
            context_override: ContextOverride::Default,
            context_history: Vec::new(),
            token_count: None,
        }
    }

    /// Create a new tool result entry with truncation metadata.
    #[must_use]
    pub fn tool_result_truncated<S1, S2>(
        id: S1,
        name: S2,
        content: String,
        full_content: String,
        status: ToolResultStatus,
        truncation: jinn_core_types::tool_types::TruncationMeta,
    ) -> Self
    where
        S1: Into<String>,
        S2: Into<String>,
    {
        Self {
            id: ChatEntryId::new(),
            timing: super::entry_timing::EntryTiming::instant_now(),
            kind: ChatEntryKind::ToolResult {
                id: id.into(),
                name: name.into(),
                content,
                status,
                full_content: Some(full_content),
                truncation: Some(truncation),
                is_alert: false,
                pin_position: None,
            },
            pin_position: None,
            context_override: ContextOverride::Default,
            context_history: Vec::new(),
            token_count: None,
        }
    }

    /// Create a new transient chat entry with the current timestamp.
    ///
    /// Transient entries are UI-only - they are excluded from prompt assembly,
    /// token estimation, and LLM context. They cannot be pinned and
    /// are not persisted.
    ///
    /// Accepts markdown text for rich formatting through the markdown renderer.
    #[must_use]
    pub fn transient<T>(text: T) -> Self
    where
        T: Into<String>,
    {
        Self {
            id: ChatEntryId::new(),
            timing: super::entry_timing::EntryTiming::instant_now(),
            kind: ChatEntryKind::Transient(text.into()),
            pin_position: None,
            context_override: ContextOverride::Default,
            context_history: Vec::new(),
            token_count: None,
        }
    }

    /// Set the pin position on this entry, returning the modified entry.
    ///
    /// Used as a builder: `ChatEntry::user("instruction").with_pin(PinPosition::Top)`
    #[must_use]
    pub fn with_pin(mut self, position: PinPosition) -> Self {
        self.pin_position = Some(position);
        self
    }

    /// Set the context override on this entry, returning the modified entry.
    ///
    /// Used as a builder: `ChatEntry::user("hello").with_context_override(ContextOverride::ForcedExclude)`
    #[must_use]
    pub fn with_context_override(mut self, override_: ContextOverride) -> Self {
        self.context_override = override_;
        self
    }

    /// Set the ignored flag on this entry, returning the modified entry.
    ///
    /// Compatibility shim for `with_context_override`. Use that method instead.
    ///
    /// Used as a builder: `ChatEntry::user("hello").with_ignored(true)`
    #[must_use]
    pub fn with_ignored(mut self, ignored: bool) -> Self {
        self.context_override = if ignored {
            ContextOverride::ForcedExclude
        } else {
            ContextOverride::Default
        };
        self
    }

    /// Construct a `ChatEntry` with the given fields and `ContextOverride::Default`.
    ///
    /// For initial creation only. To change the override later (with audit), use
    /// [`Self::apply_context_override`]. To restore from persistence (no audit), use
    /// [`Self::restore_context_override`].
    #[must_use]
    pub(crate) fn new_with_kind(
        id: ChatEntryId,
        timing: super::entry_timing::EntryTiming,
        kind: ChatEntryKind,
        pin_position: Option<PinPosition>,
    ) -> Self {
        Self {
            id,
            timing,
            kind,
            pin_position,
            context_override: ContextOverride::Default,
            context_history: Vec::new(),
            token_count: None,
        }
    }
    /// Set the context override, recording a [`ContextChangeEvent`] if and only if
    /// the new value differs from the current one.
    ///
    /// This is the single audited path for changing `context_override`. All callers -
    /// user toggles, history workers, internal sweeps - must go through this method.
    ///
    /// # Returns
    ///
    /// `true` if a change was applied (event was appended). `false` if the new value
    /// matched the current value (no-op: no event, no field change).
    pub fn apply_context_override(
        &mut self,
        new_value: ContextOverride,
        source: ChangeSource,
    ) -> bool {
        if self.context_override == new_value {
            return false;
        }
        let event = ContextChangeEvent {
            from: self.context_override,
            to: new_value,
            source,
            timestamp: jiff::Timestamp::now(),
        };
        self.context_override = new_value;
        self.context_history.push(event);
        true
    }

    /// Sets the initial context override on this entry without recording an audit
    /// event. This is intended for use only at construction time or when restoring
    /// state from persistent storage. For any other change, use
    /// [`Self::apply_context_override`] so the change is recorded in
    /// [`Self::context_history`].
    ///
    /// # Why not just write the field?
    ///
    /// The field is intentionally private to force mutations through the audited
    /// setter (`apply_context_override`). This escape hatch exists solely for
    /// initial construction (compaction summary entries) and DB loading.
    pub(crate) fn restore_context_override(&mut self, value: ContextOverride) {
        // No audit event: this method exists for initial construction and DB
        // loading, where the `context_override` value reflects an already-applied
        // state (not a new transition).
        self.context_override = value;
    }

    /// Restores the persisted token count on this entry without recomputing it.
    ///
    /// This is intended for use only when restoring state from persistent
    /// storage: the stored count is a fact about the entry's (immutable)
    /// content, not a new transition. `None` (the column was NULL) leaves the
    /// field unset for the token count actor to fill.
    ///
    /// # Why not just write the field?
    ///
    /// The count is derived from entry content, so the only legitimate writers
    /// are the token count actor (compute) and DB loading (restore). Routing
    /// the restore path through a named method keeps those two apart.
    pub(crate) fn restore_token_count(&mut self, count: Option<u32>) {
        // No audit trail: DB-restore-only path, mirroring
        // `restore_context_override`.
        self.token_count = count;
    }
    /// Read-only access to the current context override.
    ///
    /// Use this when you need to inspect the override without changing it. When
    /// changing it, use [`Self::apply_context_override`] so the change is audited.
    #[must_use]
    pub fn context_override(&self) -> ContextOverride {
        self.context_override
    }

    /// Whether this entry is pinned to the context.
    pub fn is_pinned(&self) -> bool {
        self.pin_position.is_some()
    }

    /// Whether this entry will be included in the assembled LLM prompt.
    ///
    /// Single source of truth. All consumers (assembly, gutter, minimap,
    /// token estimator, visual items) must use this method.
    ///
    /// Priority: pin > context_override > non-context defaults > kind default.
    #[must_use]
    pub fn is_in_context(&self) -> bool {
        if self.is_pinned() {
            return true;
        }
        match self.context_override() {
            ContextOverride::ForcedInclude => true,
            ContextOverride::ForcedExclude => false,
            ContextOverride::Default => {
                // Certain entries are excluded by default even though their
                // *kind* is included by default. They carry no useful context
                // and should not split contiguous hidden blocks.
                if self.is_empty_assistant() || self.is_pending_tool_result() {
                    return false;
                }
                self.kind.is_included_by_default()
            }
        }
    }

    /// Compatibility accessor: whether this entry has been forced out of context.
    ///
    /// Equivalent to `context_override == ForcedExclude`. Used during migration
    /// from `ignored: bool` to `context_override: ContextOverride`.
    ///
    /// Prefer `is_in_context()` or `context_override` directly.
    #[must_use]
    pub fn ignored(&self) -> bool {
        self.context_override() == ContextOverride::ForcedExclude
    }

    /// Whether an auto-pruner should suppress a `ForcedExclude` mutation for
    /// this entry.
    ///
    /// Returns `true` when `context_override` is either `ForcedInclude` (user
    /// or system explicitly kept the entry in context) or `ForcedExclude`
    /// (already excluded — emitting again would be a no-op duplicate). Returns
    /// `false` for `ContextOverride::Default`.
    ///
    /// This is the **auto-pruner suppression predicate**: every worker in
    /// `feat::auto_prune_worker` should consult this helper at the
    /// mutation-emission step.
    ///
    /// # Relation to other protection
    ///
    /// Pin protection (`is_pinned()`) is a **separate, older layer** and is
    /// *not* folded in here. Auto-pruners that already check `is_pinned()`
    /// continue to do so independently. The two layers compose: a pinned entry
    /// is also typically not a prune candidate (filtered earlier), but even if
    /// one reaches the emission step, both checks apply.
    #[must_use]
    pub fn is_protected_from_prune(&self) -> bool {
        matches!(
            self.context_override,
            ContextOverride::ForcedInclude | ContextOverride::ForcedExclude
        )
    }

    /// Whether this entry's most recent context change was a user-initiated
    /// `ForcedExclude`.
    ///
    /// Used by `apply_mutations` to prevent workers from re-including entries
    /// the user has explicitly excluded via the `x` key or an x-sweep.
    ///
    /// Returns `false` when `context_history` is empty (freshly constructed
    /// entries, or old data predating the audit trail feature).
    ///
    /// # Semantics
    ///
    /// Only the **most recent** audit event is consulted. If the user excluded
    /// an entry but later toggled it back to `Default`, the guard does not
    /// block re-inclusion.
    #[must_use]
    pub fn is_user_force_excluded(&self) -> bool {
        self.context_history.last().is_some_and(|event| {
            event.to == ContextOverride::ForcedExclude && event.source == ChangeSource::User
        })
    }

    /// Whether this entry is a compaction summary.
    ///
    /// Compaction entries act as delimiters in the chat history. They are
    /// never included in subsequent compaction ranges.
    #[must_use]
    pub fn is_compaction(&self) -> bool {
        matches!(self.kind, ChatEntryKind::Compaction { .. })
    }

    /// Whether this entry is a Sources (annotation) entry.
    #[must_use]
    pub fn is_annotation(&self) -> bool {
        matches!(self.kind, ChatEntryKind::Annotation { .. })
    }

    /// Whether this entry is a user message.
    #[must_use]
    pub fn is_user(&self) -> bool {
        matches!(self.kind, ChatEntryKind::User { .. })
    }

    /// Whether this entry is an Assistant entry with empty text.
    ///
    /// Empty assistant entries are created when the LLM responds with tool
    /// calls but no text. They must remain in history for correct
    /// `entries_to_messages` behavior (tool calls attach to the preceding
    /// assistant message), but should be hidden from the UI.
    #[must_use]
    pub fn is_empty_assistant(&self) -> bool {
        matches!(&self.kind, ChatEntryKind::Assistant(text) if text.is_empty())
    }

    /// Whether this is a ToolResult that never completed (Pending status).
    ///
    /// Pending results are created when a tool call starts but never
    /// receives a success or failure status. They carry incomplete data
    /// and should be treated as out-of-context.
    #[must_use]
    pub fn is_pending_tool_result(&self) -> bool {
        matches!(
            &self.kind,
            ChatEntryKind::ToolResult {
                status: ToolResultStatus::Pending,
                ..
            }
        )
    }

    /// The pin position, if this entry is pinned.
    pub fn pin_position(&self) -> Option<PinPosition> {
        self.pin_position
    }

    /// Returns a static string identifying the entry kind.
    ///
    /// Used to identify entry types without matching on the enum.
    #[must_use]
    pub fn kind_str(&self) -> &'static str {
        match self.kind {
            ChatEntryKind::User { .. } => "user",
            ChatEntryKind::System(..) => "system",
            ChatEntryKind::Error(..) => "error",
            ChatEntryKind::Assistant(..) => "assistant",
            ChatEntryKind::Actor { .. } => "actor",
            ChatEntryKind::Thinking(..) => "thinking",
            ChatEntryKind::ToolCall { .. } => "tool_call",
            ChatEntryKind::ToolResult { .. } => "tool_result",

            ChatEntryKind::Transient(..) => "transient",
            ChatEntryKind::Compaction { .. } => "compaction",
            ChatEntryKind::Annotation { .. } => "annotation",
        }
    }

    /// Returns the text content of this entry.
    ///
    /// Returns the primary text for each variant. For `Table`, returns
    /// the plain-text representation. For `ToolCall` and `ToolResult`,
    /// returns a formatted summary.
    #[must_use]
    pub fn text(&self) -> String {
        match &self.kind {
            ChatEntryKind::User { display, .. } => display.clone(),
            ChatEntryKind::System(t)
            | ChatEntryKind::Error(t)
            | ChatEntryKind::Assistant(t)
            | ChatEntryKind::Thinking(t) => t.clone(),
            ChatEntryKind::Transient(s) => s.clone(),
            ChatEntryKind::Actor { text, .. } => text.clone(),
            ChatEntryKind::ToolCall {
                name, arguments, ..
            } => {
                format!("{name}: {arguments}")
            }
            ChatEntryKind::ToolResult { name, content, .. } => {
                format!("{name}: {content}")
            }

            ChatEntryKind::Compaction { summary, .. } => summary.clone(),
            ChatEntryKind::Annotation { citations } => citations
                .iter()
                .map(|c| c.title.clone())
                .collect::<Vec<_>>()
                .join(", "),
        }
    }

    /// Returns the clipboard text for yanking this entry.
    ///
    /// Unlike [`Self::text`], tool entries yield their raw payload: a tool
    /// result yields the untruncated output (`full_content` when present)
    /// without the tool-name prefix, and a tool call yields the raw JSON
    /// arguments. ANSI escape sequences are stripped so the clipboard
    /// carries clean text (e.g. JSON that `jq` can parse directly).
    #[must_use]
    pub fn yank_text(&self) -> String {
        let text = match &self.kind {
            ChatEntryKind::ToolCall { arguments, .. } => arguments.clone(),
            ChatEntryKind::ToolResult {
                content,
                full_content,
                ..
            } => full_content.clone().unwrap_or_else(|| content.clone()),
            _ => self.text(),
        };
        strip_ansi_escapes::strip_str(&text)
    }

    /// Compute a cheap fingerprint of this entry's visual content.
    ///
    /// Two entries with the same visual content produce the same fingerprint.
    /// Used by the line count cache to detect content changes without re-rendering.
    /// The fingerprint changes when text content or entry kind changes.
    #[must_use]
    pub fn content_fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        // Hash the kind discriminant + content.
        std::mem::discriminant(&self.kind).hash(&mut hasher);
        match &self.kind {
            ChatEntryKind::User { display, .. } => display.hash(&mut hasher),
            ChatEntryKind::System(t)
            | ChatEntryKind::Error(t)
            | ChatEntryKind::Assistant(t)
            | ChatEntryKind::Thinking(t) => t.hash(&mut hasher),
            ChatEntryKind::Transient(s) => s.hash(&mut hasher),
            ChatEntryKind::Actor { text, .. } => text.hash(&mut hasher),
            ChatEntryKind::ToolCall {
                name, arguments, ..
            } => {
                name.hash(&mut hasher);
                arguments.hash(&mut hasher);
            }
            ChatEntryKind::ToolResult {
                name,
                content,
                status,
                truncation,
                ..
            } => {
                name.hash(&mut hasher);
                status.hash(&mut hasher);
                content.hash(&mut hasher);
                // Include truncation presence so the line count cache
                // invalidates when the indicator line is added or removed.
                truncation.is_some().hash(&mut hasher);
            }

            ChatEntryKind::Compaction {
                summary,
                tokens_after,
                entries_compacted,
                model_used,
                ..
            } => {
                summary.hash(&mut hasher);
                tokens_after.hash(&mut hasher);
                entries_compacted.hash(&mut hasher);
                model_used.hash(&mut hasher);
            }
            ChatEntryKind::Annotation { citations } => {
                citations.len().hash(&mut hasher);
                for c in citations {
                    c.url.hash(&mut hasher);
                    c.title.hash(&mut hasher);
                }
            }
        }
        hasher.finish()
    }
}

impl Serialize for ChatEntryKind {
    #[expect(clippy::too_many_lines, reason = "handler reads best as a single unit")]
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        use serde::ser::SerializeMap;
        match self {
            ChatEntryKind::User {
                display,
                expanded,
                attachments,
                outcome,
            } => {
                #[derive(Serialize)]
                struct UserData {
                    display: String,
                    expanded: String,
                    #[serde(default, skip_serializing_if = "Vec::is_empty")]
                    attachments: Vec<jinn_provider::Attachment>,
                    #[serde(default, skip_serializing_if = "AttachmentOutcome::is_empty")]
                    outcome: AttachmentOutcome,
                }
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry(
                    "User",
                    &UserData {
                        display: display.clone(),
                        expanded: expanded.clone(),
                        attachments: attachments.clone(),
                        outcome: outcome.clone(),
                    },
                )?;
                map.end()
            }
            ChatEntryKind::System(t) => {
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("System", t)?;
                map.end()
            }
            ChatEntryKind::Error(t) => {
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("Error", t)?;
                map.end()
            }
            ChatEntryKind::Assistant(t) => {
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("Assistant", t)?;
                map.end()
            }
            ChatEntryKind::Actor { source, text } => {
                #[derive(Serialize)]
                struct ActorData {
                    source: String,
                    text: String,
                }
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry(
                    "Actor",
                    &ActorData {
                        source: source.clone(),
                        text: text.clone(),
                    },
                )?;
                map.end()
            }
            ChatEntryKind::ToolCall {
                id,
                name,
                arguments,
                child_session,
            } => {
                #[derive(Serialize)]
                struct ToolCallData {
                    id: String,
                    name: String,
                    arguments: String,
                    #[serde(default, skip_serializing_if = "Option::is_none")]
                    child_session: Option<SessionId>,
                }
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry(
                    "ToolCall",
                    &ToolCallData {
                        id: id.clone(),
                        name: name.clone(),
                        arguments: arguments.clone(),
                        child_session: child_session.clone(),
                    },
                )?;
                map.end()
            }
            ChatEntryKind::ToolResult {
                id,
                name,
                content,
                status,
                full_content,
                truncation,
                pin_position,
                is_alert,
            } => {
                #[derive(Serialize)]
                struct ToolResultData {
                    id: String,
                    name: String,
                    content: String,
                    status: ToolResultStatus,
                    #[serde(skip_serializing_if = "Option::is_none")]
                    full_content: Option<String>,
                    #[serde(skip_serializing_if = "Option::is_none")]
                    truncation: Option<jinn_core_types::tool_types::TruncationMeta>,
                    #[serde(default, skip_serializing_if = "Option::is_none")]
                    pin_position: Option<PinPosition>,
                    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
                    is_alert: bool,
                }
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry(
                    "ToolResult",
                    &ToolResultData {
                        id: id.clone(),
                        name: name.clone(),
                        content: content.clone(),
                        status: *status,
                        full_content: full_content.clone(),
                        truncation: truncation.clone(),
                        pin_position: *pin_position,
                        is_alert: *is_alert,
                    },
                )?;
                map.end()
            }

            ChatEntryKind::Thinking(t) => {
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("Thinking", t)?;
                map.end()
            }
            ChatEntryKind::Transient(s) => {
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("Transient", s)?;
                map.end()
            }
            ChatEntryKind::Compaction {
                summary,
                tokens_before,
                tokens_after,
                entries_compacted,
                model_used,
            } => {
                #[derive(Serialize)]
                struct CompactionData {
                    summary: String,
                    tokens_before: usize,
                    tokens_after: usize,
                    entries_compacted: usize,
                    model_used: String,
                }
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry(
                    "Compaction",
                    &CompactionData {
                        summary: summary.clone(),
                        tokens_before: *tokens_before,
                        tokens_after: *tokens_after,
                        entries_compacted: *entries_compacted,
                        model_used: model_used.clone(),
                    },
                )?;
                map.end()
            }
            ChatEntryKind::Annotation { citations } => {
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("Annotation", citations)?;
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for ChatEntryKind {
    #[expect(clippy::too_many_lines, reason = "handler reads best as a single unit")]
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        use serde::de::{self, MapAccess, Visitor};
        use std::fmt;

        struct ChatEntryKindVisitor;

        impl<'de> Visitor<'de> for ChatEntryKindVisitor {
            type Value = ChatEntryKind;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a ChatEntryKind map")
            }

            #[expect(clippy::too_many_lines, reason = "handler reads best as a single unit")]
            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let key: String = map
                    .next_key()?
                    .ok_or_else(|| de::Error::missing_field("variant"))?;
                match key.as_str() {
                    "User" => {
                        #[derive(Deserialize)]
                        struct UserData {
                            display: String,
                            expanded: String,
                            #[serde(default)]
                            attachments: Vec<jinn_provider::Attachment>,
                            #[serde(default)]
                            outcome: AttachmentOutcome,
                        }
                        let data: UserData = map.next_value()?;
                        Ok(ChatEntryKind::User {
                            display: data.display,
                            expanded: data.expanded,
                            attachments: data.attachments,
                            outcome: data.outcome,
                        })
                    }
                    "System" => {
                        let text: String = map.next_value()?;
                        Ok(ChatEntryKind::System(text))
                    }
                    "Error" => {
                        let text: String = map.next_value()?;
                        Ok(ChatEntryKind::Error(text))
                    }
                    "Assistant" => {
                        let text: String = map.next_value()?;
                        Ok(ChatEntryKind::Assistant(text))
                    }
                    "Actor" => {
                        #[derive(Deserialize)]
                        struct ActorData {
                            source: String,
                            text: String,
                        }
                        let data: ActorData = map.next_value()?;
                        Ok(ChatEntryKind::Actor {
                            source: data.source,
                            text: data.text,
                        })
                    }
                    "ToolCall" => {
                        #[derive(Deserialize)]
                        struct ToolCallData {
                            id: String,
                            name: String,
                            arguments: String,
                            #[serde(default)]
                            child_session: Option<SessionId>,
                        }
                        let data: ToolCallData = map.next_value()?;
                        Ok(ChatEntryKind::ToolCall {
                            id: data.id,
                            name: data.name,
                            arguments: data.arguments,
                            child_session: data.child_session,
                        })
                    }
                    "ToolResult" => {
                        // Supports both new format (status: ToolResultStatus + truncation)
                        // and old format (success: bool) for backward compat.
                        #[derive(Deserialize)]
                        struct ToolResultDataNew {
                            id: String,
                            name: String,
                            content: String,
                            status: ToolResultStatus,
                            #[serde(default)]
                            full_content: Option<String>,
                            #[serde(default)]
                            truncation: Option<jinn_core_types::tool_types::TruncationMeta>,
                            #[serde(default)]
                            pin_position: Option<PinPosition>,
                        }
                        // Try new format first, fall back to old format.
                        #[derive(Deserialize)]
                        struct ToolResultDataOld {
                            id: String,
                            name: String,
                            content: String,
                            success: bool,
                        }
                        let value: serde_json::Value = map.next_value()?;
                        let result = serde_json::from_value::<ToolResultDataNew>(value.clone())
                            .map(|data| ChatEntryKind::ToolResult {
                                id: data.id,
                                name: data.name,
                                content: data.content,
                                status: data.status,
                                full_content: data.full_content,
                                truncation: data.truncation,
                                pin_position: data.pin_position,
                                is_alert: false,
                            })
                            .or_else(|_| {
                                serde_json::from_value::<ToolResultDataOld>(value).map(|data| {
                                    ChatEntryKind::ToolResult {
                                        id: data.id,
                                        name: data.name,
                                        content: data.content,
                                        is_alert: false,
                                        status: if data.success {
                                            ToolResultStatus::Success
                                        } else {
                                            ToolResultStatus::Failure
                                        },
                                        full_content: None,
                                        truncation: None,
                                        pin_position: None,
                                    }
                                })
                            })
                            .map_err(|e| {
                                de::Error::custom(format!("failed to deserialize ToolResult: {e}"))
                            })?;
                        Ok(result)
                    }

                    "Thinking" => {
                        let text: String = map.next_value()?;
                        Ok(ChatEntryKind::Thinking(text))
                    }
                    "Info" | "Transient" => {
                        // Transient entries are not persisted. If we encounter one in
                        // deserialized data (e.g. from an older version), treat
                        // it as System so we don't lose the text.
                        let text: String = map.next_value()?;
                        Ok(ChatEntryKind::System(text))
                    }
                    "Compaction" => {
                        #[derive(Deserialize)]
                        struct CompactionData {
                            summary: String,
                            tokens_before: usize,
                            #[serde(default)]
                            tokens_after: usize,
                            entries_compacted: usize,
                            model_used: String,
                        }
                        let data: CompactionData = map.next_value()?;
                        Ok(ChatEntryKind::Compaction {
                            summary: data.summary,
                            tokens_before: data.tokens_before,
                            tokens_after: data.tokens_after,
                            entries_compacted: data.entries_compacted,
                            model_used: data.model_used,
                        })
                    }
                    "Annotation" => {
                        let citations: Vec<jinn_provider::UrlCitation> = map.next_value()?;
                        Ok(ChatEntryKind::Annotation { citations })
                    }
                    other => Err(de::Error::unknown_variant(
                        other,
                        &[
                            "User",
                            "System",
                            "Error",
                            "Assistant",
                            "Actor",
                            "ToolCall",
                            "ToolResult",
                            "Thinking",
                            "Transient",
                            "Compaction",
                            "Annotation",
                        ],
                    )),
                }
            }
        }

        deserializer.deserialize_map(ChatEntryKindVisitor)
    }
}

impl Eq for ChatEntryKind {}

impl ChatEntryKind {
    /// Whether this entry kind is included in LLM context by default
    /// (before considering pin or user override).
    ///
    /// Kinds included by default: User, Assistant, ToolCall, ToolResult,
    /// Kinds included by default: User, Assistant, ToolCall, ToolResult, Compaction.
    ///
    /// Kinds excluded by default: Error, Thinking, Transient, System, Actor,
    /// Annotation.
    #[must_use]
    pub fn is_included_by_default(&self) -> bool {
        matches!(
            self,
            ChatEntryKind::User { .. }
                | ChatEntryKind::Assistant(..)
                | ChatEntryKind::ToolCall { .. }
                | ChatEntryKind::ToolResult { .. }
                | ChatEntryKind::Compaction { .. }
        )
    }
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

    fn fresh_user_entry() -> ChatEntry {
        ChatEntry::user("hello".to_owned())
    }

    #[rstest::rstest]
    #[test]
    fn apply_context_override_noop_returns_false() {
        let mut entry = fresh_user_entry();
        let initial_history_len = entry.context_history.len();
        let changed = entry.apply_context_override(ContextOverride::Default, ChangeSource::User);
        assert!(!changed);
        assert_eq!(entry.context_history.len(), initial_history_len);
    }

    #[rstest::rstest]
    #[test]
    fn apply_context_override_records_event_with_correct_from_to_source() {
        let mut entry = fresh_user_entry();
        let changed =
            entry.apply_context_override(ContextOverride::ForcedExclude, ChangeSource::User);
        assert!(changed);
        assert_eq!(entry.context_history.len(), 1);
        let event = &entry.context_history[0];
        assert_eq!(event.from, ContextOverride::Default);
        assert_eq!(event.to, ContextOverride::ForcedExclude);
        assert_eq!(event.source, ChangeSource::User);
    }

    #[rstest::rstest]
    #[test]
    fn apply_context_override_preserves_order_with_monotonic_timestamps() {
        let mut entry = fresh_user_entry();
        entry.apply_context_override(ContextOverride::ForcedInclude, ChangeSource::User);
        std::thread::sleep(std::time::Duration::from_millis(10));
        entry.apply_context_override(ContextOverride::ForcedExclude, ChangeSource::User);
        std::thread::sleep(std::time::Duration::from_millis(10));
        entry.apply_context_override(ContextOverride::Default, ChangeSource::User);

        assert_eq!(entry.context_history.len(), 3);
        assert!(entry.context_history[0].timestamp <= entry.context_history[1].timestamp);
        assert!(entry.context_history[1].timestamp <= entry.context_history[2].timestamp);
        assert_eq!(entry.context_history[0].to, ContextOverride::ForcedInclude);
        assert_eq!(entry.context_history[1].to, ContextOverride::ForcedExclude);
        assert_eq!(entry.context_history[2].to, ContextOverride::Default);
    }

    #[rstest::rstest]
    #[test]
    fn serde_round_trip_preserves_context_history() {
        let mut entry = fresh_user_entry();
        entry.apply_context_override(ContextOverride::ForcedExclude, ChangeSource::User);
        entry.apply_context_override(
            ContextOverride::ForcedInclude,
            ChangeSource::Worker {
                name: "test_worker".to_owned(),
            },
        );

        let json = serde_json::to_string(&entry).expect("serialize");
        let loaded: ChatEntry = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(loaded.context_history.len(), 2);
        assert_eq!(loaded.context_history[0].from, ContextOverride::Default);
        assert_eq!(loaded.context_history[0].to, ContextOverride::ForcedExclude);
        assert_eq!(loaded.context_history[0].source, ChangeSource::User);
        assert_eq!(
            loaded.context_history[1].source,
            ChangeSource::Worker {
                name: "test_worker".to_owned()
            }
        );
    }

    #[rstest::rstest]
    #[test]
    fn legacy_json_without_context_history_loads_to_empty() {
        // Start from a real entry, then strip context_history to simulate legacy data.
        let mut entry = fresh_user_entry();
        entry.apply_context_override(ContextOverride::ForcedExclude, ChangeSource::User);
        let mut json = serde_json::to_value(&entry).expect("serialize");
        let obj = json.as_object_mut().expect("object");
        obj.remove("context_history");
        let loaded: ChatEntry = serde_json::from_value(json).expect("deserialize legacy");
        assert!(loaded.context_history.is_empty());
        // The override itself is preserved.
        assert_eq!(loaded.context_override(), ContextOverride::ForcedExclude);
    }
    #[rstest::rstest]
    #[test]
    fn user_entry_with_attachments_roundtrips() {
        // Given a user entry with an image attachment.
        let mut entry = ChatEntry::user("describe this");
        entry.kind = ChatEntryKind::User {
            display: "describe this".to_owned(),
            expanded: "describe this".to_owned(),
            attachments: vec![jinn_provider::Attachment::image(
                "image/png".to_owned(),
                vec![1, 2, 3],
            )],
            outcome: AttachmentOutcome::default(),
        };

        // When serializing and deserializing.
        let json = serde_json::to_string(&entry).expect("serialize");
        let loaded: ChatEntry = serde_json::from_str(&json).expect("deserialize");

        // Then the attachment roundtrips.
        let ChatEntryKind::User { attachments, .. } = &loaded.kind else {
            panic!("expected User kind")
        };
        assert_eq!(attachments.len(), 1);
        assert!(attachments[0].is_image());
        assert_eq!(attachments[0].media_type(), "image/png");
        assert_eq!(attachments[0].data(), &[1, 2, 3]);
    }
}
