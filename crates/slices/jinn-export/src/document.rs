//! The document an export renders from.
//!
//! [`ExportDocument`] is a format-neutral projection of one session: a header
//! of metadata plus a flat, ordered list of [`ExportEntry`] values. It is
//! built from a [`jinn_session_state::snapshot::SessionSnapshot`] and knows
//! nothing about HTML or markdown — the renderers in `html.rs` and
//! `markdown.rs` are the only modules that name a concrete output language.
//!
//! Two projection rules shape it:
//!
//! - **Transient entries are absent.** They are UI-only by contract (never
//!   persisted, never sent to a model), so a welcome banner has no place in
//!   a shared transcript. Every other kind is present, in history order.
//! - **Tool bodies use [`ChatEntry::yank_text`].** That prefers
//!   `full_content` when the entry was truncated and strips ANSI escapes, so
//!   an export carries the whole payload rather than the head the terminal
//!   showed. Truncation is a display affordance and is not reproduced.

use std::path::PathBuf;

use jiff::Timestamp;
use jinn_core_types::attachment::Attachment;
use jinn_core_types::chat_entry::ChatEntry;
use jinn_core_types::chat_entry::ChatEntryKind;
use jinn_core_types::model_selection::ModelSelection;
use jinn_core_types::tool_result_status::ToolResultStatus;
use jinn_session_state::snapshot::SessionSnapshot;

/// How a tool result finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportToolStatus {
    /// The tool was still running when the session was captured.
    Pending,
    /// The tool succeeded.
    Success,
    /// The tool failed.
    Failure,
}

impl From<&ToolResultStatus> for ExportToolStatus {
    fn from(status: &ToolResultStatus) -> Self {
        match status {
            ToolResultStatus::Pending => Self::Pending,
            ToolResultStatus::Success => Self::Success,
            ToolResultStatus::Failure => Self::Failure,
        }
    }
}

impl ExportToolStatus {
    /// The lowercase word shown in a summary line.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Success => "success",
            Self::Failure => "failure",
        }
    }
}

/// An image carried by a user entry, still as raw bytes.
///
/// Renderers that can inline bytes (HTML, via a data URI) encode here;
/// renderers that cannot (markdown) fall back to a note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportImage {
    /// The image MIME type, e.g. `"image/png"`.
    pub media_type: String,
    /// The raw decoded image bytes.
    pub data: Vec<u8>,
}

impl ExportImage {
    /// The image as a `data:` URI, suitable for an `src` attribute.
    #[must_use]
    pub fn data_url(&self) -> String {
        use base64::Engine as _;
        let encoded = base64::engine::general_purpose::STANDARD.encode(&self.data);
        format!("data:{};base64,{encoded}", self.media_type)
    }
}

/// One projected chat entry.
///
/// `body` is the entry's full text for the disclosure-shaped kinds
/// (tool call, tool result, reasoning) and its rendered-source markdown for
/// the prose kinds. Renderers decide how to present it, using `class` and
/// `is_disclosure` to group it.
#[derive(Debug, Clone)]
pub struct ExportEntry {
    /// The entry's kind name, e.g. `"user"`, `"tool_call"`, `"thinking"`.
    ///
    /// Taken from [`ChatEntry::kind_str`], so a new kind flows through to
    /// the output without the renderers learning a new case.
    pub class: &'static str,
    /// The entry's body text.
    pub body: String,
    /// A one-line label for a collapsed summary, if this entry collapses.
    pub summary: Option<String>,
    /// Whether a renderer should present this entry as a disclosure.
    ///
    /// True exactly for tool calls, tool results, and reasoning. Decided
    /// once here so every renderer inherits the same grouping.
    pub is_disclosure: bool,
    /// Images attached to a user entry, in order.
    pub images: Vec<ExportImage>,
    /// A tool result's outcome, when this entry is one.
    pub tool_status: Option<ExportToolStatus>,
    /// A tool result's line count, when this entry is one.
    pub line_count: Option<usize>,
    /// The entry's estimated token count, when one was computed.
    pub token_count: Option<u32>,
    /// When the entry was created.
    pub timestamp: Timestamp,
}

/// A session's header metadata.
///
/// Everything a reader needs to place the transcript: what it is, where it
/// came from, and when it happened. The token ledger and durable task list
/// are deliberately not exported.
#[derive(Debug, Clone)]
pub struct ExportDocument {
    /// The session title, or a fallback when untitled.
    pub title: String,
    /// The session's model selection, rendered as text.
    pub model: String,
    /// The working directory the session ran in.
    pub cwd: PathBuf,
    /// When the session was created.
    pub created_at: Timestamp,
    /// When the session was last updated.
    pub updated_at: Timestamp,
    /// The session's stable identifier.
    pub session_id: String,
    /// The projected entries, in history order.
    pub entries: Vec<ExportEntry>,
}

/// A short single-line form of tool-call arguments, for a summary line.
///
/// Collapses whitespace so a multi-line JSON blob becomes one readable line
/// and truncates to `max` characters on a grapheme boundary. A renderer that
/// wants the full arguments uses the body instead.
#[must_use]
pub fn summarize_arguments(arguments: &str, max: usize) -> String {
    use unicode_segmentation::UnicodeSegmentation as _;
    let collapsed = arguments.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return String::new();
    }
    if collapsed.graphemes(true).count() <= max {
        return collapsed;
    }
    let head: String = collapsed
        .graphemes(true)
        .take(max.saturating_sub(1))
        .collect();
    format!("{head}…")
}

/// The line count of a body, for a tool-result summary line.
#[must_use]
pub fn line_count(body: &str) -> usize {
    body.lines().count()
}

/// Renders a model selection as the text a reader would recognize.
#[must_use]
pub fn model_text(model: &ModelSelection) -> String {
    match model {
        ModelSelection::Single(name) => name.clone(),
        ModelSelection::Alloy { models, .. } => models.join(", "),
    }
}

/// Projects one chat entry, or `None` when it should not be exported.
///
/// Returns `None` for [`ChatEntryKind::Transient`], which is UI-only by
/// contract. Every other kind is projected.
#[must_use]
pub fn project_entry(entry: &ChatEntry) -> Option<ExportEntry> {
    let (body, summary, is_disclosure, images, tool_status) = match &entry.kind {
        ChatEntryKind::Transient(_) => return None,
        ChatEntryKind::User {
            display,
            attachments,
            ..
        } => {
            let images = attachments
                .iter()
                .filter(|a| a.is_image())
                .map(|a| match a {
                    Attachment::Image { media_type, data } => ExportImage {
                        media_type: media_type.clone(),
                        data: data.clone(),
                    },
                })
                .collect();
            (display.clone(), None, false, images, None)
        }
        ChatEntryKind::ToolCall {
            name, arguments, ..
        } => {
            let summary = format!("{name} · {}", summarize_arguments(arguments, 72));
            (arguments.clone(), Some(summary), true, Vec::new(), None)
        }
        ChatEntryKind::ToolResult { name, status, .. } => {
            let body = entry.yank_text();
            let status = ExportToolStatus::from(status);
            let lines = line_count(&body);
            let summary = format!("{name} · {} · {lines} lines", status.as_str());
            (body, Some(summary), true, Vec::new(), Some(status))
        }
        ChatEntryKind::Thinking(_) => {
            let body = entry.yank_text();
            let summary = entry.token_count.map_or_else(
                || "reasoning".to_owned(),
                |n| format!("reasoning · ~{n} tokens"),
            );
            (body, Some(summary), true, Vec::new(), None)
        }
        ChatEntryKind::Compaction { .. } | ChatEntryKind::Annotation { .. } => {
            (entry.yank_text(), None, false, Vec::new(), None)
        }
        ChatEntryKind::System(_)
        | ChatEntryKind::Error(_)
        | ChatEntryKind::Assistant(_)
        | ChatEntryKind::Actor { .. }
        | ChatEntryKind::RuleInterrupt { .. } => (entry.yank_text(), None, false, Vec::new(), None),
    };

    let lines = line_count(&body);
    Some(ExportEntry {
        class: entry.kind_str(),
        line_count: tool_status.map(|_| lines),
        body,
        summary,
        is_disclosure,
        images,
        tool_status,
        token_count: entry.token_count,
        timestamp: entry.timing.at(),
    })
}

/// Projects a whole session snapshot into a renderable document.
#[must_use]
pub fn project_document(snapshot: &SessionSnapshot) -> ExportDocument {
    ExportDocument {
        title: snapshot
            .metadata
            .title
            .clone()
            .unwrap_or_else(|| "Untitled session".to_owned()),
        model: model_text(&snapshot.metadata.profile.model),
        cwd: snapshot.metadata.cwd.clone(),
        created_at: snapshot.metadata.created_at,
        updated_at: snapshot.metadata.updated_at,
        session_id: snapshot.metadata.session_id.to_string(),
        entries: snapshot.entries.iter().filter_map(project_entry).collect(),
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
    use jinn_core_types::chat_entry::ChatEntry;
    use jinn_core_types::context_override::ContextOverride;
    use unicode_segmentation::UnicodeSegmentation as _;

    /// Builds an entry of an arbitrary kind, mirroring what the entry
    /// constructors do for the kinds that have one.
    fn entry(kind: ChatEntryKind) -> ChatEntry {
        ChatEntry {
            id: jinn_core_types::chat_entry_id::ChatEntryId::new(),
            timing: jinn_core_types::entry_timing::EntryTiming::instant_now(),
            kind,
            pin_position: None,
            context_override: ContextOverride::Default,
            context_history: Vec::new(),
            token_count: None,
        }
    }

    #[rstest::rstest]
    fn transient_entry_is_not_exported() {
        // Given a UI-only transient entry.
        let entry = entry(ChatEntryKind::Transient("welcome".to_owned()));

        // When projecting it.
        let projected = project_entry(&entry);

        // Then nothing is projected.
        assert!(projected.is_none());
    }

    #[rstest::rstest]
    fn tool_result_exports_full_content_not_the_truncated_head() {
        // Given a tool result whose display content was truncated.
        let entry = entry(ChatEntryKind::ToolResult {
            id: "call-1".to_owned(),
            name: "read".to_owned(),
            content: "head...".to_owned(),
            status: ToolResultStatus::Success,
            full_content: Some("the whole untruncated output".to_owned()),
            truncation: None,
            pin_position: None,
            is_alert: false,
        });

        // When projecting it.
        let projected = project_entry(&entry).expect("tool results are exported");

        // Then the body is the full content, not the truncated head.
        assert_eq!(projected.body, "the whole untruncated output");
    }

    #[rstest::rstest]
    fn tool_result_without_full_content_exports_its_content() {
        // Given a tool result that was never truncated.
        let entry = entry(ChatEntryKind::ToolResult {
            id: "call-1".to_owned(),
            name: "read".to_owned(),
            content: "complete".to_owned(),
            status: ToolResultStatus::Success,
            full_content: None,
            truncation: None,
            pin_position: None,
            is_alert: false,
        });

        // When projecting it.
        let projected = project_entry(&entry).expect("tool results are exported");

        // Then the body is that content.
        assert_eq!(projected.body, "complete");
    }

    #[rstest::rstest]
    fn tool_call_is_a_disclosure_with_a_one_line_summary() {
        // Given a tool call with multi-line arguments.
        let entry = entry(ChatEntryKind::ToolCall {
            id: "call-1".to_owned(),
            name: "bash".to_owned(),
            arguments: "{\n  \"cmd\": \"ls\"\n}".to_owned(),
            child_session: None,
        });

        // When projecting it.
        let projected = project_entry(&entry).expect("tool calls are exported");

        // Then it collapses, and the summary is a single line.
        assert!(projected.is_disclosure);
        let summary = projected.summary.expect("tool calls have a summary");
        assert!(summary.starts_with("bash · "));
        assert!(!summary.contains('\n'));
    }

    #[rstest::rstest]
    fn thinking_is_a_disclosure_labelled_reasoning() {
        // Given a reasoning entry.
        let entry = entry(ChatEntryKind::Thinking("considering".to_owned()));

        // When projecting it.
        let projected = project_entry(&entry).expect("reasoning is exported");

        // Then it collapses and reads as reasoning, not thinking.
        assert!(projected.is_disclosure);
        assert_eq!(projected.class, "thinking");
        let summary = projected.summary.expect("reasoning has a summary");
        assert!(summary.starts_with("reasoning"));
    }

    #[rstest::rstest]
    fn assistant_is_not_a_disclosure() {
        // Given an assistant entry.
        let entry = entry(ChatEntryKind::Assistant("hello".to_owned()));

        // When projecting it.
        let projected = project_entry(&entry).expect("assistant is exported");

        // Then it renders inline rather than collapsed.
        assert!(!projected.is_disclosure);
        assert!(projected.summary.is_none());
    }

    #[rstest::rstest]
    fn user_images_are_carried_as_raw_bytes() {
        // Given a user entry with a one-pixel PNG attached.
        let bytes: Vec<u8> = vec![0x89, 0x50, 0x4E, 0x47];
        let entry = entry(ChatEntryKind::User {
            display: "look at this".to_owned(),
            expanded: "look at this".to_owned(),
            attachments: vec![Attachment::image("image/png".to_owned(), bytes.clone())],
            outcome: jinn_core_types::chat_entry::AttachmentOutcome::default(),
        });

        // When projecting it.
        let projected = project_entry(&entry).expect("user entries are exported");

        // Then the image rides along as bytes with its media type.
        assert_eq!(projected.images.len(), 1);
        assert_eq!(
            projected.images.first().map(|i| i.data.clone()),
            Some(bytes)
        );
        assert_eq!(
            projected.images.first().map(|i| i.media_type.clone()),
            Some("image/png".to_owned())
        );
    }

    #[rstest::rstest]
    fn argument_summary_collapses_whitespace_and_truncates() {
        // Given a long multi-line argument blob.
        let args = "{\n  \"cmd\":\n   \"a very long command line that runs on and on\"\n}";

        // When summarizing it.
        let summary = summarize_arguments(args, 40);

        // Then it is one line, truncated with an ellipsis.
        assert!(!summary.contains('\n'));
        assert!(summary.ends_with('…'));
        assert!(summary.graphemes(true).count() <= 40);
    }

    #[rstest::rstest]
    fn argument_summary_leaves_short_arguments_intact() {
        // Given a short argument blob.
        let args = r#"{"cmd":"ls"}"#;

        // When summarizing it.
        let summary = summarize_arguments(args, 72);

        // Then it is returned verbatim.
        assert_eq!(summary, r#"{"cmd":"ls"}"#);
    }
}
