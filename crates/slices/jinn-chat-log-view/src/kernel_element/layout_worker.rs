//! [`LayoutWorkerActor`] — measures a loaded session's chat log off the render thread.
//!
//! Loading a large session from disk is not what makes the UI freeze: the freeze
//! is the first frame that follows, which has to know how many wrapped lines every
//! entry occupies before it can work out the scroll extent. That measurement is
//! pure computation over the session's history, so it belongs off the main
//! thread.
//!
//! A pool of these workers shares the work. They are reached with a typed
//! `send_to_any`, which round-robins across every worker that declared
//! [`LayoutChatSession`], and the history travels as a live value rather than
//! serialized — the whole point of the message carrying the entries instead of a
//! session id.
//!
//! The measurement is deliberately the *same* arithmetic the render pass
//! performs, in [`super::history`], so the counts it publishes are the counts the
//! renderer would have computed. That is why this actor lives beside the
//! renderer rather than in the slice: the pieces that must agree — the tool
//! result pairing, the streaming and subagent-waiting flags, the wrap counting —
//! are private to the renderer, and duplicating them would guarantee drift.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::chat_log::{RenderContext, entry_to_lines};
use error_stack::Report;
use jinn_chat_log_view_msg::{
    ChatLogLayoutComputed, ContentIdentity, LayoutChatSession, MeasuredEntryCount,
    MeasuredLineCount, PREVIEW_MARKER_COLUMNS, PREVIEW_MARKER_MAX_ROWS, PROXIMITY_COUNT,
    PreviewSessionRequested, SessionPreviewRendered, VisualItem, entry_is_settled,
};
use ratatui::widgets::{Paragraph, Wrap};
use trouper::actor::{ActorPath, MsgHandler, ServiceActor};
use trouper::context::MsgCtx;
use trouper::registry::RegistryError;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::kernel_element::history::LayoutInputs;
use jinn_kernel::common::state::State;

/// Static path the layout worker pool spawns at (one pool per process).
pub const LAYOUT_WORKER_POOL_SIZE: usize = 3;

/// Workers in the preview pool.
///
/// Preview renders are a handful of lines off a five-entry tail, where a
/// measurement walks a whole transcript. Sharing one pool meant a burst of
/// cursor moves queued previews behind measurements that take seconds, and a
/// preview could sit in a full mailbox behind work that would never finish in
/// time to matter — the popup spun while its job was not merely slow but
/// effectively not scheduled. A separate pool costs one more actor and makes
/// the preview's latency independent of the chat log's.
pub const PREVIEW_WORKER_POOL_SIZE: usize = 2;

/// The measurement pool's path prefix; worker `n` spawns at `jinn.chat_log.layout.worker.{n}`.
pub const LAYOUT_WORKER_PATH_PREFIX: &str = "jinn.chat_log.layout.worker.";

/// The preview pool's path prefix; worker `n` spawns at `jinn.chat_log.layout.preview.{n}`.
pub const PREVIEW_WORKER_PATH_PREFIX: &str = "jinn.chat_log.layout.preview.";

/// The path the layout worker at `index` spawns at.
#[must_use]
pub fn layout_worker_path(index: usize) -> ActorPath {
    ActorPath::new(format!("{LAYOUT_WORKER_PATH_PREFIX}{index}"))
}

/// The path the preview worker at `index` spawns at.
#[must_use]
pub fn preview_worker_path(index: usize) -> ActorPath {
    ActorPath::new(format!("{PREVIEW_WORKER_PATH_PREFIX}{index}"))
}

/// Dependencies for [`LayoutWorkerActor`].
#[derive(Clone)]
pub struct LayoutWorkerActorDeps {
    /// Shared application state, for the per-entry render inputs.
    pub state: State,
}

/// Measures a session's chat log and publishes the resulting line counts.
pub struct LayoutWorkerActor {
    /// Shared application state.
    state: State,
}

impl ServiceActor for LayoutWorkerActor {
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "ServiceActor::start is async by trait contract"
    )]
    async fn start(_args: &trouper::json::Json) -> Result<Self, Report<RegistryError>> {
        // Never called: spawned via `spawn`'s start_with (typed deps can't
        // ride the JSON args).
        Err(Report::new(RegistryError::InvalidSpec)
            .attach("LayoutWorkerActor spawns via start_with"))
    }
}

impl LayoutWorkerActor {
    /// Spawns one worker of the layout pool at `index`.
    ///
    /// Every worker declares the same work message, which is what lets
    /// `send_to_any` distribute a job across the pool.
    ///
    /// # Panics
    ///
    /// Panics if the actor's path is already taken — a wiring bug.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "port convention: spawn takes owned deps and clones into start_with"
    )]
    pub fn spawn(
        system: &trouper::system::ActorSystem,
        index: usize,
        deps: LayoutWorkerActorDeps,
    ) -> ActorPath {
        let path = layout_worker_path(index);
        trouper::builder::spawn_service_builder::<Self>(system)
            .at(path.clone())
            .start_with({
                let deps = deps.clone();
                move || {
                    let deps = deps.clone();
                    Box::pin(async move { Ok(Self { state: deps.state }) })
                }
            })
            .handles::<LayoutChatSession>()
            // The result leaves through ctx.publish; the flush gate drops any
            // outbound type not declared here.
            .emits::<ChatLogLayoutComputed>()
            .mailbox(64, trouper::inbox::OverloadPolicy::Block)
            .start();
        path
    }

    /// Spawns one worker of the preview pool at `index`.
    ///
    /// Separate from [`Self::spawn`] because the two kinds of work have wildly
    /// different costs: a preview wraps five entries, a measurement walks a
    /// whole transcript. Sharing a pool let the expensive work starve the cheap
    /// one, which is the wrong way round for a UI that must feel instant.
    ///
    /// # Panics
    ///
    /// Panics if the actor's path is already taken — a wiring bug.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "port convention: spawn takes owned deps and clones into start_with"
    )]
    pub fn spawn_preview(
        system: &trouper::system::ActorSystem,
        index: usize,
        deps: LayoutWorkerActorDeps,
    ) -> ActorPath {
        let path = preview_worker_path(index);
        trouper::builder::spawn_service_builder::<Self>(system)
            .at(path.clone())
            .start_with({
                let deps = deps.clone();
                move || {
                    let deps = deps.clone();
                    Box::pin(async move { Ok(Self { state: deps.state }) })
                }
            })
            .handles::<PreviewSessionRequested>()
            .emits::<SessionPreviewRendered>()
            .mailbox(64, trouper::inbox::OverloadPolicy::Block)
            .start();
        path
    }
}

impl MsgHandler<LayoutChatSession> for LayoutWorkerActor {
    async fn handle(&mut self, msg: &LayoutChatSession, ctx: &mut MsgCtx<'_>) {
        // The per-entry render inputs (expanded set, streaming tool calls,
        // subagent phases) live in application state, so they are snapshotted
        // once here rather than threaded through the message.
        let inputs = LayoutInputs::snapshot(&self.state.read(), &msg.session_id);

        // Measuring a large history is seconds of pure CPU. Off the actor's
        // thread so a long job cannot stall the pool's other messages.
        //
        // The handler only lends the message, and a `spawn_blocking` closure
        // must own everything it touches, so a job is moved across. It is a
        // handle, not a copy: the entries are already an `Arc` shared with the
        // load actor, so taking one for the blocking thread is a pointer bump
        // and only the small id-keyed set is duplicated.
        let job = MeasureJob::from(msg);
        let measured = tokio::task::spawn_blocking(move || measure(&job, &inputs))
            .await
            .unwrap_or_default();

        ctx.publish(ChatLogLayoutComputed {
            session_id: msg.session_id.clone(),
            content_width: msg.content_width,
            counts: measured
                .into_iter()
                .map(|count| MeasuredEntryCount {
                    entry_id: count.id,
                    signature: count.content.signature,
                    fingerprint: count.content.fingerprint,
                    is_expanded: count.is_expanded,
                    variant: count.variant,
                    wrapped_count: count.wrapped_count,
                })
                .collect(),
        });
    }
}

/// Renders the tail of a session's history as preview lines.
///
/// The *same* [`entry_to_lines`] arithmetic the chat-log measure pass performs,
/// at a narrower width and with the per-entry render inputs pinned off — a
/// preview is a glance at a conversation, not a view of it, so nothing in it is
/// selected, expanded, or streaming. That pinning is what makes it safe to share
/// one implementation with the layout worker: the two agree on how a line is
/// built and differ only in what they do with the result.
///
/// An entry still accumulating tokens is *not* rendered. It is replaced, at its
/// own position, by a bounded continuation marker — see [`continuation_marker`]
/// — so a preview neither re-renders a reply on every token nor lets a reply in
/// production vanish. Settled entries are the most recent ones rather than the
/// trailing ones, so an in-production entry at the end does not shrink the
/// window.
///
/// The window is opened by *lines*, not by entries. `max_entries` is a bound on
/// how far back the walk may reach, not a claim about how much it can show: a
/// history whose messages are tall fills on the line budget almost immediately,
/// and one whose messages are one line each keeps reaching back until the budget
/// is met. A fixed entry count cannot fill a fixed line budget — one long
/// message inside five entries overruns `max_lines` on its own, gets drained
/// from the front, and leaves the preview a few rows short with a gap above them,
/// and five one-line messages cannot fill twenty rows between them.
///
/// Overflow is dropped from the *front*: the last line is the one the user is
/// looking for, and a preview truncated at the end would show them the oldest
/// text in the window.
#[must_use]
pub fn render_preview(
    entries: &[jinn_core_types::ChatEntry],
    ctx: &RenderContext,
    max_entries: usize,
    max_lines: usize,
) -> Vec<ratatui::text::Line<'static>> {
    let mut lines: Vec<ratatui::text::Line<'static>> = Vec::new();
    let mut settled_seen = 0usize;

    // Walking backwards, newest first, prepending as we go. Two things stop the
    // walk: the line budget, and the reach bound. Which binds depends on the
    // history, and neither is allowed to leave the preview short when the other
    // could still fill it.
    for entry in entries.iter().rev() {
        if settled_seen >= max_entries {
            break;
        }
        if entry_is_settled(entry) {
            settled_seen += 1;
        }
        let rendered: Vec<ratatui::text::Line<'static>> = if entry_is_settled(entry) {
            entry_to_lines(entry, ctx)
        } else {
            continuation_marker(entry, ctx, max_lines)
        };
        // Whether the budget was already met *before* this entry, so the last
        // entry to reach the budget is still rendered and only the rows above it
        // are dropped. Testing the total alone would drop the whole entry and
        // lose its newest rows.
        let was_full = lines.len() >= max_lines;
        lines.splice(..0, rendered);
        if was_full || lines.len() >= max_lines {
            break;
        }
    }

    if lines.len() > max_lines {
        lines.drain(..lines.len() - max_lines);
    }
    lines
}

/// The continuation marker standing in for an entry still in production.
///
/// Bounded twice over, because both bounds are about the popup rather than the
/// text. [`PREVIEW_MARKER_COLUMNS`] of rendered columns decides how much of the
/// tail is shown, so a reply in production cannot cost unbounded wrap work; the
/// text is *not* markdown-rendered, which is both cheaper and honest — markdown
/// syntax mid-stream is not yet meaningful, and the point of the marker is that
/// the reply is still going, not what it will look like.
///
/// A leading `…` says text was dropped from the front; a trailing `…` says the
/// marker itself was cut at the column bound. Both directions matter: without
/// the leading one a marker would look like the whole reply, and without the
/// trailing one it would look like an arbitrary slice.
///
/// The row budget is [`PREVIEW_MARKER_MAX_ROWS`], and never more than the
/// preview's own line budget — a marker is a status line, not content, and a
/// reply in production must not be able to push the settled entries it is
/// standing in for out of the popup. Overflow is dropped from the *front* of the
/// marker, so what survives is the newest part of the reply, which is the part
/// that says what it is about to finish saying.
fn continuation_marker(
    entry: &jinn_core_types::ChatEntry,
    ctx: &RenderContext,
    max_lines: usize,
) -> Vec<ratatui::text::Line<'static>> {
    let style = ratatui::style::Style::default().fg(ctx.theme.muted_text);
    let text = entry.text();
    // An entry that has not produced any text yet still has to show *something*:
    // a zero-line marker is an invisible entry, and a session that has only just
    // begun would preview as blank. The single `…` says "here, and still going".
    if text.trim().is_empty() {
        return vec![ratatui::text::Line::from(ratatui::text::Span::styled(
            "\u{2026}", style,
        ))];
    }

    let (tail, dropped_front) = tail_columns(&text, PREVIEW_MARKER_COLUMNS);
    let leading = if dropped_front { "\u{2026}" } else { "" };
    // The bound is a column budget, so it is enforced after wrapping: a wrapped
    // row that is exactly the content width is the unit the reader sees, and
    // cutting mid-row would split a word the marker just decided to keep.
    let row_budget = PREVIEW_MARKER_MAX_ROWS.min(max_lines).max(1);
    let mut rows: Vec<String> = Vec::new();
    let mut width = 0usize;
    for row in wrap_rows(tail, ctx.content_width) {
        if rows.len() == row_budget || width + row.width() > PREVIEW_MARKER_COLUMNS {
            break;
        }
        width += row.width();
        rows.push(row.into_owned());
    }
    // A marker cut at the column bound or at its row budget ends in `…`, so the
    // reader can tell a bounded tail from the whole reply.
    let cut = rows.len() < count_wrapped_rows(tail, ctx.content_width);
    if rows.is_empty() {
        return vec![ratatui::text::Line::from(ratatui::text::Span::styled(
            "\u{2026}", style,
        ))];
    }
    if cut && let Some(last) = rows.last_mut() {
        last.push('\u{2026}');
    }
    rows.into_iter()
        .map(|row| {
            ratatui::text::Line::from(ratatui::text::Span::styled(
                format!("{leading}{row}"),
                style,
            ))
        })
        .collect()
}

/// The last `max_columns` rendered columns of `text`, and whether anything was
/// dropped from the front to make them fit.
///
/// Measured in display columns rather than bytes or chars so a reply written in
/// a wide script is cut at the same *visible* point as a reply in ASCII. The
/// walk goes backwards over graphemes, because a grapheme is the smallest unit
/// that can be dropped without leaving a broken glyph behind.
fn tail_columns(text: &str, max_columns: usize) -> (&str, bool) {
    if text.width() <= max_columns {
        return (text, false);
    }
    let mut width = 0usize;
    let mut start = text.len();
    for (offset, grapheme) in text.grapheme_indices(true).rev() {
        let grapheme_width = grapheme.width();
        if width + grapheme_width > max_columns {
            break;
        }
        width += grapheme_width;
        start = offset;
    }
    // `start` came from `grapheme_indices`, so it is on a character boundary by
    // construction. `get` rather than a slice so that fact is checked rather than
    // merely asserted: an index that somehow landed mid-character would yield no
    // marker at all, which is a visible bug, rather than a panic on the render
    // thread.
    (text.get(start..).unwrap_or_default(), true)
}

/// `text` split into rows no wider than `width`, breaking at the width rather
/// than at word boundaries.
///
/// A marker's text is a stream of tokens, not prose: mid-token it is mostly one
/// very long word, so a word-aware wrapper would put the whole thing on one row
/// and leave the column bound to do the cutting. Breaking at the width keeps the
/// number of rows predictable, which is what the row budget needs.
fn wrap_rows(text: &str, width: u16) -> Vec<Cow<'_, str>> {
    let width = usize::from(width);
    if width == 0 {
        return vec![Cow::Borrowed(text)];
    }
    let mut rows = Vec::new();
    let mut row = String::new();
    let mut row_width = 0usize;
    for grapheme in text.graphemes(true) {
        let grapheme_width = grapheme.width().max(1);
        if row_width + grapheme_width > width {
            rows.push(Cow::Owned(std::mem::take(&mut row)));
            row_width = 0;
        }
        row.push_str(grapheme);
        row_width += grapheme_width;
    }
    if !row.is_empty() || rows.is_empty() {
        rows.push(Cow::Owned(row));
    }
    rows
}

/// How many rows `text` wraps to at `width` — the count [`wrap_rows`] would
/// produce, computed without materializing the rows.
fn count_wrapped_rows(text: &str, width: u16) -> usize {
    let width = usize::from(width);
    if width == 0 {
        return 1;
    }
    let mut rows = 1usize;
    let mut row_width = 0usize;
    for grapheme in text.graphemes(true) {
        let grapheme_width = grapheme.width().max(1);
        if row_width + grapheme_width > width {
            rows += 1;
            row_width = 0;
        }
        row_width += grapheme_width;
    }
    rows
}

/// Renders a session preview's lines and publishes them.
///
/// The sidebar popup draws these every frame, and building them inline would put
/// a wrap of five entries on the render thread — the same cost the measurement
/// above exists to avoid. A preview rides the *same* pool for the same reason:
/// it is the same arithmetic with the same inputs, differing only in what it does
/// with the result. A third pool would have duplicated the spawn, path prefix,
/// and supervision for a ten-line difference.
impl MsgHandler<PreviewSessionRequested> for LayoutWorkerActor {
    async fn handle(&mut self, msg: &PreviewSessionRequested, ctx: &mut MsgCtx<'_>) {
        // The theme is read here rather than carried: it is a field wide, and a
        // request outlives the moment it was made (the pool may be busy), so
        // whatever the user last selected is the one worth rendering with.
        let theme = self.state.read().frontend.theme.clone();
        let job = PreviewJob::from(msg);

        let lines = tokio::task::spawn_blocking(move || {
            let ctx = RenderContext {
                content_width: job.content_width,
                is_selected: false,
                is_expanded: false,
                tool_entry_max_lines: job.tool_entry_max_lines,
                theme,
                paired_status: None,
                is_streaming: false,
                is_waiting_on_subagent: false,
            };
            render_preview(
                &job.entries,
                &ctx,
                jinn_chat_log_view_msg::PREVIEW_ENTRY_COUNT,
                jinn_chat_log_view_msg::PREVIEW_MAX_LINES,
            )
        })
        .await
        .unwrap_or_default();

        ctx.publish(SessionPreviewRendered {
            session_id: msg.session_id.clone(),
            generation: msg.generation,
            signature: msg.signature,
            content_width: msg.content_width,
            lines: Arc::new(lines),
        });
    }
}

/// Everything one preview render reads, detached from the bus message.
///
/// Shaped like [`MeasureJob`] for the same reason: the history moves to a
/// blocking thread as a shared `Arc`, never a second transcript.
pub(crate) struct PreviewJob {
    /// The session's trailing entries, shared with the sidebar that requested
    /// them.
    ///
    /// Already trimmed to the preview window by the requester, which would
    /// otherwise copy a whole long history per keystroke for entries
    /// [`render_preview`] slices away anyway.
    pub entries: Arc<[jinn_core_types::ChatEntry]>,
    /// Content width to wrap at.
    pub content_width: u16,
    /// Lines before a tool call or result is truncated.
    pub tool_entry_max_lines: u16,
}

impl From<&PreviewSessionRequested> for PreviewJob {
    fn from(msg: &PreviewSessionRequested) -> Self {
        Self {
            // Bounded here, at the one place a request becomes work, rather than
            // in `render_preview`: the preview displays at most its last 20 rows
            // and its in-production entries only a 256-column marker, so text
            // past [`PREVIEW_ENTRY_TEXT_BYTES`] can never be shown. Trimming it
            // first is what keeps a multi-megabyte reply from being walked by the
            // markdown renderer for lines that are then thrown away.
            entries: bounded_entries(&msg.entries),
            content_width: msg.content_width,
            tool_entry_max_lines: msg.tool_entry_max_lines,
        }
    }
}

/// Bytes of an entry's own text a preview render is allowed to look at.
///
/// A bound on work, not a display rule: a preview shows at most its last 20
/// rows, and a settled entry's text is wrapped to the popup's width before the
/// front is dropped, so nothing beyond a few kilobytes of tail can survive to
/// the screen.
const PREVIEW_ENTRY_TEXT_BYTES: usize = 4096;

/// The carried entries, each one's own text cut to its last
/// [`PREVIEW_ENTRY_TEXT_BYTES`].
///
/// Only the text an entry *owns* is trimmed, and the cut lands on a UTF-8
/// character boundary — a preview must not panic on a multi-byte reply, and
/// cutting mid-character would produce exactly that. Everything else about an
/// entry (its id, kind, timing) is preserved, so a settled entry still renders
/// and an in-production one still gets its marker.
///
/// An entry whose text is already inside the bound is left completely alone,
/// including its identity: cloning it unchanged keeps the common case free of
/// any work beyond the check.
fn bounded_entries(
    entries: &Arc<[jinn_core_types::ChatEntry]>,
) -> Arc<[jinn_core_types::ChatEntry]> {
    if entries
        .iter()
        .all(|entry| entry.text().len() <= PREVIEW_ENTRY_TEXT_BYTES)
    {
        return Arc::clone(entries);
    }
    Arc::from(
        entries
            .iter()
            .map(|entry| {
                let text = entry.text();
                if text.len() <= PREVIEW_ENTRY_TEXT_BYTES {
                    return entry.clone();
                }
                let mut bounded = entry.clone();
                truncate_entry_text(&mut bounded, &text);
                bounded
            })
            .collect::<Vec<_>>(),
    )
}

/// Replace `entry`'s own text with the tail of `text`, on a character boundary.
///
/// The kind-specific fields are what a renderer reads, so the tail is written
/// back into each of them rather than into some parallel "preview text" — an
/// entry the renderer cannot read is not a bounded entry.
fn truncate_entry_text(entry: &mut jinn_core_types::ChatEntry, text: &str) {
    let tail = text_tail_bytes(text, PREVIEW_ENTRY_TEXT_BYTES);
    match &mut entry.kind {
        jinn_core_types::ChatEntryKind::Assistant(t)
        | jinn_core_types::ChatEntryKind::Thinking(t)
        | jinn_core_types::ChatEntryKind::System(t)
        | jinn_core_types::ChatEntryKind::Error(t)
        | jinn_core_types::ChatEntryKind::Transient(t)
        | jinn_core_types::ChatEntryKind::Actor { text: t, .. } => t.clone_from(&tail),
        jinn_core_types::ChatEntryKind::ToolCall { arguments, .. } => {
            arguments.clone_from(&tail);
        }
        jinn_core_types::ChatEntryKind::ToolResult { content, .. } => {
            content.clone_from(&tail);
        }
        jinn_core_types::ChatEntryKind::User {
            display, expanded, ..
        } => {
            // Both fields hold the same text and both are cut: one is what a
            // renderer reads, the other is what the model would be sent, and
            // trimming only one would leave the entry describing itself two ways.
            // `clone_from` reuses the field's existing allocation, which is the
            // point — this runs per entry on a path that exists to bound work.
            display.clone_from(&tail);
            expanded.clone_from(&tail);
        }
        // A compaction summary and an annotation's citation titles have no single
        // text field a bound could be written to. Both are far shorter than the
        // bound by construction — a summary is capped when it is written and a
        // citation list is a list of titles — so they are left as they are.
        jinn_core_types::ChatEntryKind::Compaction { .. }
        | jinn_core_types::ChatEntryKind::Annotation { .. } => {}
        // Rule guidance is a short authored block, bounded well under the
        // preview limit, so there is nothing to trim.
        jinn_core_types::ChatEntryKind::RuleInterrupt { .. } => {}
    }
}

/// The last `max_bytes` of `text`, starting at a UTF-8 character boundary.
///
/// The boundary walk is the whole function: a `&str` sliced at an arbitrary
/// byte is not a `&str`, and a preview that panicked on a multi-megabyte
/// emoji-bearing reply would take the render down with it.
fn text_tail_bytes(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    let mut start = text.len() - max_bytes;
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    text.get(start..).unwrap_or_default().to_owned()
}

/// Measures every entry's wrapped line count for one session.
///
/// Reproduces the render pass's arithmetic exactly: the same visual item
/// projection, the same tool result pairing, the same wrap counting. The
/// rendered lines are discarded as soon as they are counted, so a measurement
/// costs no lasting memory beyond the counts themselves.
pub(crate) fn measure(job: &MeasureJob, inputs: &LayoutInputs) -> Vec<MeasuredLineCount> {
    let visual_items = jinn_chat_log_view_msg::build_visual_items(
        &job.entries,
        &job.shown_ignored_blocks,
        PROXIMITY_COUNT,
        job.min_collapse_count,
    );
    let tool_result_statuses = pair_tool_results(&job.entries);

    let mut measured = Vec::with_capacity(visual_items.len());
    for item in &visual_items {
        let VisualItem::Entry(history_index) = item else {
            // A collapsed block is always exactly one line, so there is
            // nothing to measure and nothing to store.
            continue;
        };
        let Some(entry) = job.entries.get(*history_index) else {
            continue;
        };
        measured.push(measure_entry(entry, &tool_result_statuses, inputs, job));
    }
    measured
}

/// Everything one measurement reads, detached from the bus message.
///
/// The entries are held as a shared `Arc`, so assembling a job for a blocking
/// thread is a pointer bump rather than a second transcript. Only the small
/// id-keyed set is copied.
pub(crate) struct MeasureJob {
    /// The session's history, shared with whoever else already holds it.
    pub entries: Arc<[jinn_core_types::ChatEntry]>,
    /// Blocks of ignored entries the user has expanded.
    pub shown_ignored_blocks: HashSet<jinn_core_types::ChatEntryId>,
    /// Minimum contiguous ignored entries required to collapse a block.
    pub min_collapse_count: usize,
    /// Content width to measure at.
    pub content_width: u16,
    /// Lines before a tool call or result is truncated.
    pub tool_entry_max_lines: u16,
}

impl From<&LayoutChatSession> for MeasureJob {
    fn from(msg: &LayoutChatSession) -> Self {
        Self {
            entries: Arc::clone(&msg.entries),
            shown_ignored_blocks: msg.shown_ignored_blocks.clone(),
            min_collapse_count: msg.min_collapse_count,
            content_width: msg.content_width,
            tool_entry_max_lines: msg.tool_entry_max_lines,
        }
    }
}

/// Measures a single entry's wrapped line count.
fn measure_entry(
    entry: &jinn_core_types::ChatEntry,
    tool_result_statuses: &HashMap<String, jinn_core_types::ToolResultStatus>,
    inputs: &LayoutInputs,
    job: &MeasureJob,
) -> MeasuredLineCount {
    use jinn_core_types::ChatEntryKind;

    let is_expanded = inputs.is_expanded(&entry.id);
    let paired_status = match &entry.kind {
        ChatEntryKind::ToolCall { id, .. } => tool_result_statuses.get(id).copied(),
        ChatEntryKind::ToolResult { status, .. } => Some(*status),
        _ => None,
    };
    let is_streaming = inputs.is_streaming(entry);
    let is_waiting_on_subagent = inputs.is_task_waiting(entry, tool_result_statuses);

    let ctx = RenderContext {
        content_width: job.content_width,
        is_selected: false,
        is_expanded,
        tool_entry_max_lines: job.tool_entry_max_lines,
        theme: inputs.theme().clone(),
        paired_status,
        is_streaming,
        is_waiting_on_subagent,
    };
    let lines = entry_to_lines(entry, &ctx);
    let wrapped_count = wrapped_line_count(&lines, job.content_width);

    MeasuredLineCount {
        id: entry.id.clone(),
        content: ContentIdentity {
            signature: entry.content_signature(),
            fingerprint: entry.content_fingerprint(),
        },
        is_expanded,
        variant: crate::kernel_element::history::render_variant(
            paired_status,
            is_streaming,
            is_waiting_on_subagent,
        ),
        wrapped_count,
    }
}

/// How many wrapped lines `lines` occupies at `content_width`.
///
/// A width of zero means "do not wrap", matching the render pass.
fn wrapped_line_count(lines: &[ratatui::text::Line<'static>], content_width: u16) -> u32 {
    if content_width == 0 {
        return u32::try_from(lines.len()).unwrap_or(u32::MAX);
    }
    Paragraph::new(lines.to_vec())
        .wrap(Wrap { trim: false })
        .line_count(content_width) as u32
}

/// Pairs each tool result with its call so the renderer can tint the call's
/// background by the result's status.
fn pair_tool_results(
    entries: &[jinn_core_types::ChatEntry],
) -> HashMap<String, jinn_core_types::ToolResultStatus> {
    entries
        .iter()
        .filter_map(|entry| match &entry.kind {
            jinn_core_types::ChatEntryKind::ToolResult { id, status, .. } => {
                Some((id.clone(), *status))
            }
            _ => None,
        })
        .collect()
}
