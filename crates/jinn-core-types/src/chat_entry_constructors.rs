//! The per-kind constructors for [`ChatEntry`].
//!
//! Every variant of [`ChatEntryKind`] has exactly one constructor here, and each
//! is generic over its text-bearing fields so a caller can pass anything that
//! renders to a string without an intermediate allocation. They are the
//! supported way to build an entry: the kind enum is public, so nothing stops a
//! caller from writing the struct literal, but these are what set the
//! invariants every other code path relies on — a tool result always carries a
//! name and a status, a user entry always carries a timestamp.
//!
//! This is separated from the entry's own accessors because construction and
//! inspection are different questions. Everything else on `ChatEntry` answers
//! "what does this entry look like"; these answer "make me one".

use super::chat_entry::{AttachmentOutcome, ChatEntry, ChatEntryKind};
use super::chat_entry_id::ChatEntryId;
use super::context_override::ContextOverride;
use super::tool_result_status::ToolResultStatus;

impl ChatEntry {
    pub fn user<T>(text: T) -> Self
    where
        T: Into<String>,
    {
        let t = text.into();
        Self {
            id: ChatEntryId::new(),
            timing: crate::entry_timing::EntryTiming::instant_now(),
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
            ChatEntryKind::RuleInterrupt { body, .. } => Some(body.as_str()),
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
            timing: crate::entry_timing::EntryTiming::instant_now(),
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
            timing: crate::entry_timing::EntryTiming::instant_now(),
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
            timing: crate::entry_timing::EntryTiming::instant_now(),
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
            timing: crate::entry_timing::EntryTiming::instant_now(),
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
            timing: crate::entry_timing::EntryTiming::instant_now(),
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
            timing: crate::entry_timing::EntryTiming::instant_now(),
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
            timing: crate::entry_timing::EntryTiming::instant_now(),
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
            timing: crate::entry_timing::EntryTiming::instant_now(),
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
    pub fn annotation(citations: Vec<crate::url_citation::UrlCitation>) -> Self {
        Self {
            id: ChatEntryId::new(),
            timing: crate::entry_timing::EntryTiming::instant_now(),
            kind: ChatEntryKind::Annotation { citations },
            pin_position: None,
            context_override: ContextOverride::Default,
            context_history: Vec::new(),
            token_count: None,
        }
    }

    /// Create a rule interrupt entry for a fired stream rule.
    ///
    /// The entry carries guidance the harness injected mid-turn. It reaches the
    /// model as a user turn — interception is useless if the model never sees the
    /// guidance — but it renders as its own kind so the user can tell harness
    /// steering apart from their own input.
    #[must_use]
    pub fn rule_interrupt<S1, S2>(rule: S1, body: S2) -> Self
    where
        S1: Into<String>,
        S2: Into<String>,
    {
        Self {
            id: ChatEntryId::new(),
            timing: crate::entry_timing::EntryTiming::instant_now(),
            kind: ChatEntryKind::RuleInterrupt {
                rule: rule.into(),
                body: body.into(),
            },
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
        truncation: crate::tool_types::TruncationMeta,
    ) -> Self
    where
        S1: Into<String>,
        S2: Into<String>,
    {
        Self {
            id: ChatEntryId::new(),
            timing: crate::entry_timing::EntryTiming::instant_now(),
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
            timing: crate::entry_timing::EntryTiming::instant_now(),
            kind: ChatEntryKind::Transient(text.into()),
            pin_position: None,
            context_override: ContextOverride::Default,
            context_history: Vec::new(),
            token_count: None,
        }
    }
}
