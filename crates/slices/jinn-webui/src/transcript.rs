//! The transcript projection and render functions.
//!
//! [`TranscriptModel`] is the webui-owned projection of one session's
//! history: a cursor-stable ordered list of rendered entries plus the
//! metadata a page shows around them. It follows the export slice's
//! projection rules — transient entries are absent (UI-only by
//! contract), tool bodies use [`ChatEntry::yank_text`] (prefers
//! `full_content`, strips ANSI) — because a web transcript mirrors the
//! terminal and export already solved the projection half.
//!
//! Rendering rules, in one place:
//!
//! - **Markdown** (`Assistant`, `User.display`) renders server-side
//!   through [`render_markdown`], whose event rewrite demotes raw HTML
//!   in the source to text — a model-written `<script>` stays inert.
//! - **Everything else** (tool arguments, tool results, thinking,
//!   actor, system) is escaped verbatim — never interpreted.
//! - Tool call + its result render as one `<details>` disclosure per
//!   entry; the export's disclosure classification is inherited via
//!   the same kind set.
//!
//! Cursor stability: an entry's index is its position in the rendered
//! sequence, and entries are only ever appended to a live history's
//! tail in the phases a stream produces, so `since=N` returning
//! entries at `N..` stays coherent between pull and push.

use jinn_core_types::SessionId;
use jinn_core_types::chat_entry::ChatEntry;
use jinn_core_types::chat_entry::ChatEntryKind;
use jinn_core_types::tool_result_status::ToolResultStatus;
use pulldown_cmark::Event;
use pulldown_cmark::Options;
use pulldown_cmark::Parser;

use crate::contract::ATTACHMENT_PREFIX;

/// Escapes text for inclusion in HTML — the one escaping function the
/// slice renders through. Fragments built anywhere else call this, so
/// a `<script>` in a tool output or a token delta can never become
/// live markup.
#[must_use]
pub fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

/// Renders markdown source to HTML.
///
/// Raw HTML in the source is escaped rather than passed through, so a
/// `<script>` written by a model (or pasted into a message) stays
/// inert text in the page. Everything else is delegated to
/// pulldown-cmark's own renderer, which escapes text nodes.
#[must_use]
pub fn render_markdown(source: &str) -> String {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);

    // Raw HTML in the source is demoted to a text node rather than
    // passed through; the whole event stream is rewritten first and
    // rendered in one pass, so the library's writer keeps a single
    // consistent state.
    let events = Parser::new_ext(source, options).map(|event| match event {
        Event::Html(text) => Event::Text(text),
        other => other,
    });
    let mut out = String::with_capacity(source.len() + source.len() / 4);
    pulldown_cmark::html::push_html(&mut out, events);
    out
}

/// One rendered transcript entry: the projection's output, a
/// template's input.
#[derive(Debug, Clone)]
pub struct RenderedEntry {
    /// The entry's kind name — the CSS class and the template's
    /// grouping key.
    pub class: &'static str,
    /// Pre-rendered HTML body: markdown where the kind is prose,
    /// escaped text everywhere else.
    pub body_html: String,
    /// One-line collapsed summary, for disclosure entries.
    pub summary: Option<String>,
    /// Whether the template presents this entry as a `<details>`.
    pub is_disclosure: bool,
    /// A tool result's outcome word (`success`/`failure`/`pending`),
    /// when this entry is one.
    pub tool_status: Option<&'static str>,
    /// The entry's creation time, ISO-8601.
    pub at: String,
}

/// The web projection of one session's history plus page metadata.
#[derive(Debug, Clone)]
pub struct TranscriptModel {
    /// The session title, or the untitled fallback.
    pub title: String,
    /// The model selection, rendered as text.
    pub model: String,
    /// The lowercase phase name (idle / streaming / working / cancelling).
    pub phase: String,
    /// Whether the session is a bot run.
    pub is_bot: bool,
    /// The rendered entries, in history order. Index = cursor position.
    pub entries: Vec<RenderedEntry>,
}

impl TranscriptModel {
    /// Projects a live session under an already-held read guard.
    ///
    /// The guard is the caller's (borrowed, not taken) so a handler can
    /// project and clone within one consistent snapshot and drop it
    /// before any await.
    #[must_use]
    pub fn from_live(
        history: &[ChatEntry],
        title: Option<&str>,
        model: &str,
        phase: &str,
        is_bot: bool,
        session_id: &SessionId,
    ) -> Self {
        Self {
            title: title.unwrap_or("Untitled Session").to_owned(),
            model: model.to_owned(),
            phase: phase.to_owned(),
            is_bot,
            entries: render_history(history, session_id),
        }
    }

    /// Projects a store snapshot — the archived-session path. The same
    /// entry projection as [`Self::from_live`]; only the metadata
    /// source differs.
    #[must_use]
    pub fn from_snapshot(
        snapshot: &jinn_session_state::snapshot::SessionSnapshot,
        title: Option<&str>,
        model: &str,
        phase: &str,
        is_bot: bool,
        session_id: &SessionId,
    ) -> Self {
        Self {
            title: title.unwrap_or("Untitled Session").to_owned(),
            model: model.to_owned(),
            phase: phase.to_owned(),
            is_bot,
            entries: render_history(&snapshot.entries, session_id),
        }
    }

    /// The entry count — the cursor's coordinate space.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing is rendered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The entries at `cursor..`, or the whole transcript when the
    /// cursor is out of range — the shrink-re-read rule: history can
    /// shorten under a rewind-retry, and a stale cursor must re-sync,
    /// not receive nothing.
    #[must_use]
    pub fn since(&self, cursor: Option<usize>) -> &[RenderedEntry] {
        match cursor.and_then(|cursor| self.entries.get(cursor..)) {
            Some(tail) => tail,
            None => &self.entries,
        }
    }
}

/// Appends one `<img>` per attachment beneath the entry text, each served
/// by the immutable attachment route — never a data: URI (the fragments
/// must stay byte-small; the settle push would otherwise carry every
/// image on every convergence).
fn append_attachment_images(
    body: &mut String,
    entry: &ChatEntry,
    session_id: &SessionId,
    attachments: &[jinn_provider::Attachment],
) {
    use std::fmt::Write as _;
    for (ordinal, attachment) in attachments.iter().enumerate() {
        let media_type = escape_html(attachment.media_type());
        let _ = write!(
            body,
            "<img class=\"attachment\" src=\"{ATTACHMENT_PREFIX}{session_id}/{}/{}\" alt=\"attached {media_type}\">",
            entry.id, ordinal,
        );
    }
}

/// Projects one chat entry into its rendered form.
///
/// Every kind renders (see [`is_renderable`]): disclosures for tool
/// calls/results and thinking, prose for everything else, and
/// transients as muted one-shot notices (the refresh summary table).
#[must_use]
pub fn render_entry(entry: &ChatEntry, session_id: &SessionId) -> RenderedEntry {
    let (class, body, summary, is_disclosure, tool_status) = match &entry.kind {
        ChatEntryKind::User {
            display,
            attachments,
            ..
        } => {
            // Attached images render inline beneath the text, each served
            // by the immutable attachment route — never a data: URI (the
            // fragments must stay byte-small; the settle push would
            // otherwise carry every image on every convergence).
            let mut body = render_markdown(display);
            append_attachment_images(&mut body, entry, session_id, attachments);
            ("user", body, None, false, None)
        }
        ChatEntryKind::Assistant(text) => ("assistant", render_markdown(text), None, false, None),
        ChatEntryKind::Actor { source, text } => (
            "actor",
            escape_html(text),
            Some(format!("actor · {source}")),
            false,
            None,
        ),
        ChatEntryKind::System(text) => ("system", escape_html(text), None, false, None),
        ChatEntryKind::Error(text) => ("error", escape_html(text), None, false, None),
        ChatEntryKind::Thinking(_) => {
            let body = entry.yank_text();
            let summary = entry.token_count.map_or_else(
                || "reasoning".to_owned(),
                |n| format!("reasoning · ~{n} tokens"),
            );
            ("thinking", escape_html(&body), Some(summary), true, None)
        }
        ChatEntryKind::ToolCall {
            name, arguments, ..
        } => {
            let summary = format!("{name} · {}", summarize_arguments(arguments, 72));
            (
                "tool_call",
                escape_html(arguments),
                Some(summary),
                true,
                None,
            )
        }
        ChatEntryKind::ToolResult { name, status, .. } => {
            let body = entry.yank_text();
            let status_word = status_word(status);
            let lines = body.lines().count();
            let summary = format!("{name} · {status_word} · {lines} lines");
            (
                "tool_result",
                escape_html(&body),
                Some(summary),
                true,
                Some(status_word),
            )
        }
        // Compaction summaries and annotations are prose the reader
        // should see inline; intersession and rule-interrupt notes are
        // escaped verbatim. Transients are absent.
        ChatEntryKind::Compaction { .. } => (
            "compaction",
            escape_html(&entry.yank_text()),
            None,
            false,
            None,
        ),
        ChatEntryKind::Annotation { .. } => (
            "annotation",
            escape_html(&entry.yank_text()),
            None,
            false,
            None,
        ),
        ChatEntryKind::RuleInterrupt { .. } => (
            "rule_interrupt",
            escape_html(&entry.yank_text()),
            None,
            false,
            None,
        ),
        ChatEntryKind::Intersession { .. } => (
            "intersession",
            escape_html(&entry.yank_text()),
            None,
            false,
            None,
        ),
        ChatEntryKind::Transient(text) => {
            // UI-only notice (the refresh summary table and friends) —
            // rendered like the terminal renders it, muted via CSS.
            ("transient", escape_html(text), None, false, None)
        }
    };

    RenderedEntry {
        class,
        body_html: body,
        summary,
        is_disclosure,
        tool_status,
        at: entry.timing.at().to_string(),
    }
}

/// Whether an entry participates in the rendered sequence.
///
/// Every entry kind renders — the web transcript shows what the
/// terminal shows, including transients (the refresh summary table).
/// The predicate stays as the one place the participation rule is
/// stated; callers keep filtering through it so the rule cannot drift
/// apart between pull and push.
///
/// # Note
///
/// Transients are UI-only: the session store filters them out of
/// SQLite persistence and export ignores them — this predicate governs
/// **web rendering only**.
#[must_use]
pub const fn is_renderable(_entry: &ChatEntry) -> bool {
    true
}

/// The lowercase outcome word for a tool result.
#[must_use]
pub const fn status_word(status: &ToolResultStatus) -> &'static str {
    match status {
        ToolResultStatus::Pending => "pending",
        ToolResultStatus::Success => "success",
        ToolResultStatus::Failure => "failure",
    }
}

/// A short single-line form of tool-call arguments, for a summary line.
///
/// Collapses whitespace so a multi-line JSON blob becomes one readable
/// line and truncates on a grapheme boundary — the export slice's
/// summary rule, copied.
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

/// Filters and renders a history slice — the one entry loop.
#[must_use]
pub fn render_history(history: &[ChatEntry], session_id: &SessionId) -> Vec<RenderedEntry> {
    history
        .iter()
        .filter(|e| is_renderable(e))
        .map(|entry| render_entry(entry, session_id))
        .collect()
}
