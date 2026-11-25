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
//!   in the source to text — a model-written `<script>` stays inert —
//!   links bare `http(s)` addresses found in prose, and makes every
//!   kept link open in a new tab under `noopener`.
//! - **Everything else** (tool arguments, tool results, thinking,
//!   actor, system) is escaped verbatim — never interpreted, so an
//!   address there is text, not a link.
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
use pulldown_cmark::CowStr;
use pulldown_cmark::Event;
use pulldown_cmark::Options;
use pulldown_cmark::Parser;
use pulldown_cmark::Tag;

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
/// inert text in the page. Bare `http://`/`https://` addresses in prose
/// become links (see [`split_bare_urls`]); every markdown link with an
/// `http(s)` destination opens in a new tab under `noopener`, and a
/// link to any other scheme renders as its label text alone, never an
/// anchor. Footnotes are enabled: model-written `[^n]` references and
/// definitions render as real elements (`sup.footnote-reference`,
/// `div.footnote-definition`) instead of one literal text block.
#[must_use]
pub fn render_markdown(source: &str) -> String {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_FOOTNOTES);

    // The whole event stream is rewritten first and rendered in one
    // pass, so the library's writer keeps a single consistent state.
    let events = Parser::new_ext(source, options)
        .scan(MarkdownRewrite::default(), |rewrite, event| {
            Some(rewrite.next(event))
        });
    let mut out = String::with_capacity(source.len() + source.len() / 4);
    pulldown_cmark::html::push_html(&mut out, events);
    out
}

/// The event rewriter [`render_markdown`] runs between the parser and
/// the HTML writer.
///
/// It carries the nesting of links, images, and code blocks the stream
/// is currently inside — per open link, whether its opening tag was
/// emitted as an anchor — and rewrites each event:
///
/// - `Html` is demoted to a text node, so a model-written `<script>`
///   stays inert.
/// - `Text` outside every link, image, and code block is split on bare
///   addresses. A code block's contents arrive as `Text` events too,
///   so the block itself is what exempts them.
/// - `Start(Link)` to an `http(s)` destination is replaced by an anchor
///   carrying `target="_blank" rel="noopener"`; one to any other scheme
///   is dropped, so only the label text renders.
#[derive(Debug, Default)]
struct MarkdownRewrite {
    /// One entry per link or image the stream is inside, in open order:
    /// `true` when its opening tag was emitted as an anchor.
    open_links: Vec<bool>,
    /// The nesting depth of code blocks the stream is inside.
    code_block_depth: usize,
}

impl MarkdownRewrite {
    /// Rewrites one event.
    fn next<'a>(&mut self, event: Event<'a>) -> Event<'a> {
        match event {
            Event::Html(text) => Event::Text(text),
            Event::Start(Tag::CodeBlock(..)) => {
                self.code_block_depth += 1;
                event
            }
            Event::End(Tag::CodeBlock(..)) => {
                self.code_block_depth = self.code_block_depth.saturating_sub(1);
                event
            }
            Event::Start(Tag::Link(_, dest_url, _)) => self.start_link(&dest_url),
            Event::End(Tag::Link(..)) => self.end_link(),
            Event::Start(Tag::Image(..)) => {
                self.open_links.push(false);
                event
            }
            Event::End(Tag::Image(..)) => {
                self.open_links.pop();
                event
            }
            Event::Text(text) if self.is_prose() => linkify_text(&text),
            other => other,
        }
    }

    /// A link's opening tag: an anchor for an `http(s)` destination,
    /// nothing at all for any other scheme.
    fn start_link(&mut self, dest_url: &str) -> Event<'static> {
        let is_anchor = is_linkable_url(dest_url);
        self.open_links.push(is_anchor);
        if is_anchor {
            Event::Html(anchor_start_html(dest_url).into())
        } else {
            Event::Text(CowStr::Borrowed(""))
        }
    }

    /// A link's closing tag: `</a>` when the opening tag was an anchor.
    fn end_link(&mut self) -> Event<'static> {
        match self.open_links.pop() {
            Some(true) => Event::Html(CowStr::Borrowed("</a>")),
            _ => Event::Text(CowStr::Borrowed("")),
        }
    }

    /// Whether a text node is ordinary prose — outside every link,
    /// image, and code block, the only place bare addresses linkify.
    fn is_prose(&self) -> bool {
        self.open_links.is_empty() && self.code_block_depth == 0
    }
}

/// Splits one text node on its bare addresses and renders the result:
/// anchors for the addresses, escaped text for everything else.
fn linkify_text(text: &CowStr<'_>) -> Event<'static> {
    let mut html = String::with_capacity(text.len());
    for piece in split_bare_urls(text) {
        match piece {
            TextPiece::Plain(plain) => html.push_str(&escape_html(plain)),
            TextPiece::Link { url, trailing } => {
                html.push_str(&bare_url_html(url));
                html.push_str(&escape_html(trailing));
            }
        }
    }
    Event::Html(html.into())
}

/// An anchor's opening tag for a markdown link destination.
fn anchor_start_html(dest_url: &str) -> String {
    use std::fmt::Write as _;
    let url = escape_html(dest_url);
    let mut out = String::with_capacity(url.len() + 64);
    let _ = write!(out, "<a href=\"{url}\" target=\"_blank\" rel=\"noopener\">");
    out
}

/// One stretch of a markdown text node after bare-URL detection.
#[derive(Debug, PartialEq, Eq)]
enum TextPiece<'a> {
    /// Source text with no linkable address in it.
    Plain(&'a str),
    /// A bare `http://`/`https://` address that passed validation.
    Link {
        /// The address, exactly as written in the source.
        url: &'a str,
        /// Characters trimmed off the address's end — sentence
        /// punctuation or an unmatched closing bracket. They render
        /// as text right after the link so nothing is lost.
        trailing: &'a str,
    },
}

/// Splits one markdown text node on its bare `http://`/`https://`
/// addresses.
///
/// An address runs to the next whitespace; its end is then walked back
/// over sentence punctuation (`. , ; : ! ?` and quotes) and any closing
/// bracket the address does not open, and what remains must parse as a
/// URL. A candidate that fails the gate renders as plain text. Interior
/// characters are never touched, so the commas of a map coordinate and
/// the `?` of a query string survive intact.
#[must_use]
fn split_bare_urls(text: &str) -> Vec<TextPiece<'_>> {
    const SCHEMES: [&str; 2] = ["http://", "https://"];

    let mut pieces = Vec::new();
    let mut rest = text;
    while let Some((before, link, after)) = next_bare_url(rest, &SCHEMES) {
        if !before.is_empty() {
            pieces.push(TextPiece::Plain(before));
        }
        pieces.push(link);
        rest = after;
    }
    if !rest.is_empty() {
        pieces.push(TextPiece::Plain(rest));
    }
    pieces
}

/// The first linkable bare address in `text`, as the text before it, the
/// link piece, and the text after its run.
///
/// A scheme match that is not at a word start (`xhttps://`) or whose
/// candidate fails the gate is skipped — the scan resumes one byte past
/// the `h`, which is a char boundary because every scheme begins with
/// ASCII. Every slice below goes through a checked split, so a
/// non-boundary can only end the scan, never panic.
fn next_bare_url<'a>(text: &'a str, schemes: &[&str]) -> Option<(&'a str, TextPiece<'a>, &'a str)> {
    let mut search_from = 0;
    while let Some(scheme_at) = next_scheme(text, search_from, schemes) {
        let (before, candidate) = text.split_at_checked(scheme_at)?;
        let is_word_start = before.is_empty() || before.ends_with(char::is_whitespace);
        let Some(trimmed) = is_word_start.then(|| trim_candidate(candidate)).flatten() else {
            search_from = scheme_at + 1;
            continue;
        };
        return Some((
            before,
            TextPiece::Link {
                url: trimmed.url,
                trailing: trimmed.trailing,
            },
            trimmed.rest,
        ));
    }
    None
}

/// The earliest of the schemes in `text` at or after `from`.
fn next_scheme(text: &str, from: usize, schemes: &[&str]) -> Option<usize> {
    let tail = text.get(from..)?;
    schemes
        .iter()
        .filter_map(|scheme| tail.find(scheme).map(|at| from + at))
        .min()
}

/// A bare-URL candidate after its end has been walked back to the
/// address proper.
#[derive(Debug, PartialEq, Eq)]
struct TrimmedCandidate<'a> {
    /// The address proper.
    url: &'a str,
    /// What was trimmed off its end.
    trailing: &'a str,
    /// The source after the whole run.
    rest: &'a str,
}

/// Walks a candidate's end back over sentence punctuation and unmatched
/// closing brackets, then applies the URL-parse gate.
///
/// Returns `None` when the candidate fails the gate — the caller leaves
/// it as text and resumes the scan past the scheme.
fn trim_candidate(candidate: &str) -> Option<TrimmedCandidate<'_>> {
    let (run, rest) = split_at_whitespace(candidate);
    let (url, trailing) = run.split_at_checked(address_end(run))?;
    is_linkable_url(url).then_some(TrimmedCandidate {
        url,
        trailing,
        rest,
    })
}

/// Splits `text` at its first whitespace, with the whitespace landing at
/// the head of the tail.
fn split_at_whitespace(text: &str) -> (&str, &str) {
    match text.find(char::is_whitespace) {
        Some(at) => text.split_at(at),
        None => (text, ""),
    }
}

/// Whether a character ends a sentence and so belongs to the prose
/// around an address rather than to the address itself.
fn is_sentence_punctuation(ch: char) -> bool {
    matches!(
        ch,
        '.' | ',' | ';' | ':' | '!' | '?' | '"' | '\'' | '>' | '<'
    )
}

/// Splits `run` into the address proper and the characters trimmed off
/// its end.
///
/// Sentence punctuation and quotes are trimmed unconditionally. A
/// closing bracket is trimmed only when the run holds more closers than
/// openers of its kind, so `Foo_(bar)` keeps its `)` while an address
/// wrapped in prose parens loses the outer one.
fn address_end(run: &str) -> usize {
    run.trim_end_matches(is_sentence_punctuation)
        .trim_end_matches(|ch: char| is_unmatched_closer(ch, run))
        .len()
}

/// Whether `ch` is a closing bracket with no matching opener in `run`.
fn is_unmatched_closer(ch: char, run: &str) -> bool {
    let (opener, closer) = match ch {
        ')' => ('(', ')'),
        ']' => ('[', ']'),
        '}' => ('{', '}'),
        _ => return false,
    };
    let openers = run.chars().filter(|c| *c == opener).count();
    let closers = run.chars().filter(|c| *c == closer).count();
    closers > openers
}

/// The URL-parse gate: whether `text` is a well-formed http(s) address.
///
/// The scheme check runs first because the parser accepts many schemes
/// (`javascript:` and `data:` among them) that must never become links.
fn is_linkable_url(text: &str) -> bool {
    let is_http =
        url_starts_with_scheme(text, "http://") || url_starts_with_scheme(text, "https://");
    is_http && url::Url::parse(text).is_ok()
}

/// Renders a bare address as an anchor opening in a new tab.
fn bare_url_html(url: &str) -> String {
    use std::fmt::Write as _;
    let url = escape_html(url);
    let mut out = String::with_capacity(url.len() * 2 + 64);
    let _ = write!(
        out,
        "<a href=\"{url}\" target=\"_blank\" rel=\"noopener\">{url}</a>"
    );
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
    /// Whether the disclosure renders open — the in-flight tail's
    /// thinking entries only. The settled transcript renders every
    /// disclosure collapsed; the streaming tail renders thinking open
    /// (the turn's reasoning is the thing being watched) and every
    /// other disclosure collapsed (TUI parity).
    pub is_streaming: bool,
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
/// `is_streaming` names the disclosure state the template applies —
/// false here (the settled projection); the streaming tail's render
/// sets it for its thinking entries.
#[must_use]
pub fn render_entry(entry: &ChatEntry, session_id: &SessionId) -> RenderedEntry {
    render_entry_streaming(entry, session_id, false)
}

/// [`render_entry`] with the streaming disclosure state decided: the
/// in-flight tail renders thinking entries open; everything else
/// stays collapsed regardless of state.
#[must_use]
pub fn render_entry_streaming(
    entry: &ChatEntry,
    session_id: &SessionId,
    streaming_open: bool,
) -> RenderedEntry {
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
        // Sources render as one collapsed disclosure per entry — the
        // TUI's "Sources (N)" block's web shape: the summary carries
        // the count, the body holds one linked row per citation (see
        // [`sources_html`]). Intersession and rule-interrupt notes are
        // escaped verbatim; transients are absent.
        ChatEntryKind::Annotation { citations } => {
            let count = citations.len();
            (
                "annotation",
                sources_html(citations),
                Some(format!("Sources ({count})")),
                true,
                None,
            )
        }
        ChatEntryKind::Compaction { .. } => (
            "compaction",
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
        is_streaming: is_disclosure && streaming_open && class == "thinking",
        tool_status,
        at: entry.timing.at().to_string(),
    }
}

/// Renders one citation row: the linked title (or the URL when the
/// title is empty — the TUI's untitled fallback) with the URL as
/// muted text beside it. Only `http(s)` URLs become links; every
/// model- or provider-supplied value is escaped, and a link renders
/// `target="_blank"` with `rel="noopener"` so the source opens in a
/// new tab without handing it the opener handle. Snippets are not
/// displayed (TUI parity: title + URL only).
fn citation_row_html(citation: &jinn_core_types::url_citation::UrlCitation) -> String {
    use std::fmt::Write as _;
    let title_text = if citation.title.is_empty() {
        "(untitled)"
    } else {
        citation.title.as_str()
    };
    let title = escape_html(title_text);
    let url = escape_html(&citation.url);
    let mut row = String::with_capacity(title.len() + url.len() + 96);
    if url_starts_with_scheme(&citation.url, "http://")
        || url_starts_with_scheme(&citation.url, "https://")
    {
        let _ = write!(
            row,
            "<div class=\"citation\"><a class=\"citation-title\" href=\"{url}\" \
             target=\"_blank\" rel=\"noopener\">{title}</a> \
             <span class=\"citation-url\">{url}</span></div>"
        );
    } else {
        let _ = write!(
            row,
            "<div class=\"citation\"><span class=\"citation-title\">{title}</span> \
             <span class=\"citation-url\">{url}</span></div>"
        );
    }
    row
}

/// Whether the text starts with the given scheme, ASCII-case-
/// insensitively. The render re-checks the URL's shape rather than
/// trusting the detector — a hostile or malformed payload renders as
/// plain text, never as a link.
fn url_starts_with_scheme(url: &str, scheme: &str) -> bool {
    url.as_bytes()
        .get(..scheme.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(scheme.as_bytes()))
}

/// Renders the sources list — one [`citation_row_html`] per citation,
/// in the entry's arrival order (the detector's dedupe order).
fn sources_html(citations: &[jinn_core_types::url_citation::UrlCitation]) -> String {
    let mut out = String::new();
    for citation in citations {
        out.push_str(&citation_row_html(citation));
    }
    out
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
