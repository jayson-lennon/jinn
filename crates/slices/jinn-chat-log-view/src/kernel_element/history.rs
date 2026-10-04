//! Renders the conversation history.
//!
//! Each entry in the chat log is displayed with a distinct visual style so the user
//! can tell them apart at a glance:
//!
//! - **User messages** appear as white text on a dark gray background block.
//! - **System messages** appear muted in dark gray.
//! - **Actor messages** appear highlighted with the actor's name and content.
//! - **Assistant messages** appear in white with no background.
//! - **Tool calls** appear as dark text on a dark green background block.
//! - **Tool results** appear as dark text on a dark green (success) or dark red
//!   (failure) background block.
//!
//! A 2-column gutter on the left shows a dark gray background by default,
//! and turns yellow when the cursor selects an entry. Pinned entries show
//! a 📌 emoji in the gutter. When a pinned entry is selected, the gutter
//! background changes to the focus accent color (yellow by default) so the
//! pin highlight is unmistakable.
//!
//! The gutter is rendered as a separate column from the content so that
//! line wrapping does not break the gutter display.
//!
//! Text wraps within the available space.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use jinn_chat_log_view_msg::{
    DEFAULT_MIN_COLLAPSE_COUNT, PROXIMITY_COUNT, VisualItem, build_visual_items,
};
use jinn_core_types::EntryTiming;
use jinn_core_types::SessionId;
use jinn_kernel::common::app_state::AppState;
use jinn_kernel::common::render_ctx::RenderCtx;
use jinn_kernel::common::ui_element::UiElement;
use jinn_kernel::protocol::ToolResultStatus;
use jinn_kernel::protocol::{ChatEntry, ChatEntryId, ChatEntryKind};
use jinn_session_msg::PhaseKind;
use jinn_session_state::ChatSessionState;
use jinn_slices::DrawContext;
use jinn_theme::Theme;
use jinn_tools_msg::TASK_TOOL_NAME;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use super::loading_indicator::LoadingIndicator;
use crate::chat_log::{
    GUTTER_WIDTH, GutterStyle, RenderContext, ScrollState, build_blank_gutter_lines,
    build_collapsed_block_gutter_line, build_entry_gutter_lines, compute_scroll, entry_to_lines,
    find_visible_indices, render_scroll_indicator,
};
use jinn_chat_log_view_msg::EntryLineCache;
use jinn_preferences_config::schemas::ChatLogConfig;

/// Default number of lines to show for tool entries (calls and results) before truncating.
const DEFAULT_TOOL_ENTRY_MAX_LINES: u16 = 6;

/// Shortest gap between two renders of the streaming entry.
///
/// One frame at the TUI's redraw cadence. It is not configurable: if the
/// interval ever needs to change, it changes here and nowhere else.
pub(crate) const STREAM_RENDER_INTERVAL: Duration = Duration::from_millis(16);

// alternatives: |❚┃╏⣿𜺏░▒▓
const GUTTER_STR: &str = "𜺏 ";

/// Hash of the status-derived render inputs that change an entry's rendered
/// lines without changing its content fingerprint (paired tool-result
/// background tint, streaming flag, subagent-waiting line). Used as the
/// render-variant component of the entry line cache key so the cache
/// invalidates when any of them flips.
///
/// Shared with the off-thread layout worker, which must produce byte-identical
/// variants or every count it publishes would miss.
pub(crate) fn render_variant(
    paired_status: Option<ToolResultStatus>,
    is_streaming: bool,
    is_waiting_on_subagent: bool,
) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (paired_status, is_streaming, is_waiting_on_subagent).hash(&mut hasher);
    hasher.finish()
}

/// The per-entry render inputs that live in application state rather than in
/// the history itself.
///
/// Snapshotted once per layout job so the off-thread measurement sees a stable
/// set: the render pass gathers the same three things every frame, and a
/// measurement taken across a state change would produce counts that no frame
/// could ever hit.
pub(crate) struct LayoutInputs {
    /// Theme colors, cloned once per job rather than once per entry.
    theme: Theme,
    /// Entries whose tool result content is expanded.
    expanded: HashSet<ChatEntryId>,
    /// Tool call entries still streaming their arguments.
    streaming: HashSet<ChatEntryId>,
    /// Child sessions loaded and actively running, by session id.
    running_children: HashSet<SessionId>,
}

impl LayoutInputs {
    /// Snapshots the layout inputs for one session.
    pub(crate) fn snapshot(state: &AppState, session_id: &SessionId) -> Self {
        use jinn_session_msg::PhaseKind;

        let running_children = state
            .session
            .iter()
            .filter(|(_, child)| matches!(child.phase(), PhaseKind::Sending | PhaseKind::Streaming))
            .map(|(id, _)| id.clone())
            .collect();

        let session = state.session.get(session_id);
        Self {
            theme: state.frontend.theme.clone(),
            expanded: session.map_or_else(HashSet::new, ChatSessionState::expanded_entry_ids),
            streaming: session.map_or_else(HashSet::new, ChatSessionState::streaming_tool_call_ids),
            running_children,
        }
    }

    /// Whether this entry's tool result content is expanded.
    pub(crate) fn is_expanded(&self, id: &ChatEntryId) -> bool {
        self.expanded.contains(id)
    }

    /// The theme to render this job's entries with.
    pub(crate) fn theme(&self) -> &Theme {
        &self.theme
    }

    /// Whether this tool call's arguments render through the streaming path.
    ///
    /// Takes the entry rather than its id because the abandoned-partial case
    /// reads the entry's timing and context membership; the live case is a set
    /// lookup. Delegates to [`is_streaming_tool_call`] so the render pass and
    /// the layout worker cannot disagree.
    pub(crate) fn is_streaming(&self, entry: &ChatEntry) -> bool {
        is_streaming_tool_call(entry, &self.streaming)
    }

    /// Whether this `task` call is waiting on a loaded, running child session.
    pub(crate) fn is_task_waiting(
        &self,
        entry: &ChatEntry,
        tool_result_statuses: &HashMap<String, ToolResultStatus>,
    ) -> bool {
        let ChatEntryKind::ToolCall {
            id,
            name,
            child_session,
            ..
        } = &entry.kind
        else {
            return false;
        };
        if name != TASK_TOOL_NAME {
            return false;
        }
        // A paired result means the tool already finished.
        if tool_result_statuses.contains_key(id) {
            return false;
        }
        child_session
            .as_ref()
            .is_some_and(|child| self.running_children.contains(child))
    }
}

/// Display element for the full conversation history.
#[derive(Debug, Default)]
pub struct ChatLogElement {
    /// The loading status line shown while a session loads.
    loading: LoadingIndicator,
    /// The streaming entry's previous rendering, carried between frames so a
    /// throttled frame can paint the previous text instead of re-deriving it.
    ///
    /// Held on the element rather than in `paint` because the element is one
    /// long-lived instance registered with the registry: a local would not
    /// survive to the next frame, and would throttle nothing.
    stream_lines: Option<StreamedLines>,
}

/// The previous frame's rendering of the streaming entry.
///
/// Reused verbatim while a frame is throttled. Dropped as soon as the entry
/// stops streaming, so a settled entry can never paint from it.
#[derive(Debug)]
struct StreamedLines {
    /// The entry these lines belong to.
    id: ChatEntryId,
    /// The wrapped line count they occupy.
    wrapped_count: u32,
    /// The rendered lines themselves.
    lines: Arc<Vec<Line<'static>>>,
    /// When they were rendered.
    rendered_at: Instant,
}

impl ChatLogElement {
    /// Create a new chat log element.
    #[must_use]
    pub fn new() -> Self {
        Self {
            loading: LoadingIndicator::default(),
            stream_lines: None,
        }
    }

    /// Whether the streaming entry is due a re-render.
    ///
    /// Every streamed token changes that entry's content fingerprint, so its
    /// cache entry misses and the whole entry re-renders inline: markdown
    /// parse, every code block re-highlighted, a wrap count, and a clone.
    /// Bounding that to one render per frame interval keeps the redraw
    /// cadence — which is what the eye tracks — rather than the token rate.
    pub(crate) fn stream_render_due(&self) -> bool {
        self.stream_lines
            .as_ref()
            .is_none_or(|streamed| streamed.rendered_at.elapsed() >= STREAM_RENDER_INTERVAL)
    }

    /// The throttled lines reusable for `id`, if any.
    ///
    /// `None` whenever there is no previous render to reuse, which sends the
    /// caller down the real render path — suppression is an optimisation and
    /// must never be the reason an entry has no lines to paint.
    pub(crate) fn reusable_stream_lines(
        &self,
        id: &ChatEntryId,
    ) -> Option<(u32, Arc<Vec<Line<'static>>>)> {
        let streamed = self.stream_lines.as_ref()?;
        (streamed.id == *id).then(|| (streamed.wrapped_count, Arc::clone(&streamed.lines)))
    }

    /// Record the streaming entry's rendering, stamped with this instant.
    fn record_stream_lines(
        &mut self,
        id: ChatEntryId,
        wrapped_count: u32,
        lines: Arc<Vec<Line<'static>>>,
    ) {
        self.stream_lines = Some(StreamedLines {
            id,
            wrapped_count,
            lines,
            rendered_at: Instant::now(),
        });
    }

    /// Forget the streaming entry's rendering.
    fn forget_stream_lines(&mut self) {
        self.stream_lines = None;
    }
}

impl UiElement for ChatLogElement {
    fn name(&self) -> String {
        "chat-log".to_owned()
    }

    fn is_selectable(&self) -> bool {
        true
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx) {
        self.paint(frame, area, ctx as &dyn DrawContext<AppState>);
    }
}

impl ChatLogElement {
    /// Paints the conversation history into `area`.
    ///
    /// The registered draw function calls this against the slice's one
    /// element instance, so the loading throbber's animation state
    /// advances across frames rather than being rebuilt per frame.
    ///
    /// Takes the draw context as a trait object: the registry is keyed
    /// on the state type so it can live in a `'static` cell, and this
    /// is the only context the render pass builds.
    pub fn paint(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &dyn DrawContext<AppState>) {
        let state = ctx.state();
        if state.session.is_loading() {
            self.loading.paint(frame, area, &state.frontend.theme);
            return;
        }

        let mut render = HistoryRender::new(state, area, ctx.config());
        render.compute_visual_items();
        render.build_tool_result_map();

        // The entry accumulating assistant content is the one entry whose
        // content changes every frame, so it is the only one worth throttling.
        // `None` when nothing is streaming, which disables the throttle
        // entirely — settled history always renders when asked to.
        let streaming_id = render.streaming_entry_id();
        let throttled: Option<(u32, Arc<Vec<Line<'static>>>)> = match &streaming_id {
            Some(id) if self.stream_render_due() => None,
            Some(id) => self.reusable_stream_lines(id),
            None => {
                // Streaming stopped; never paint a settled entry from this.
                self.forget_stream_lines();
                None
            }
        };

        if let Some(cell) = state.frontend.line_cache_cell() {
            cell.update(|cache| {
                render.compute_line_ranges(cache, streaming_id.as_ref(), throttled.as_ref());
                render.compute_scroll();

                {
                    let session = state.active_session();
                    session.set_last_max_offset(render.scroll.max_offset);
                    session.set_entry_line_ranges_if_changed(&render.entry_line_ranges);
                    session.set_viewport_height(area.height);
                    session.set_blank_count(render.scroll.blank_count as u32);
                    session.set_rendered_scroll_offset(render.scroll.clamped);
                    // Published so a session loaded later measures at the width
                    // this frame used, instead of being measured at a guessed
                    // width and thrown away as stale.
                    session.set_content_width(render.content_width);
                    // The same arrangement for the collapse threshold: the
                    // coverage probe runs off the render thread and must build
                    // the same visual items this frame built.
                    session.set_min_collapse_count(render.min_collapse_count);
                }

                render.find_visible_indices();
                render.build_blank_lines();
                render.render_visible_entries(cache);
            });

            // Only an actual render stamps the interval, and only for the
            // entry that was actually rendered. A frame that fell through to
            // render still produces reusable lines, so it stamps too.
            if let (Some(id), Some((wrapped_count, lines))) =
                (streaming_id, render.stream_rendered())
            {
                self.record_stream_lines(id, wrapped_count, lines);
            }
        }
        render.paint(frame);
    }
}

// ---------------------------------------------------------------------------
// Measurement coverage
// ---------------------------------------------------------------------------

/// Whether every line the chat log would draw for `session_id` is already
/// measured at `content_width`.
///
/// The frontend calls this before switching to a session, to decide whether the
/// switch needs a measurement dispatched or can happen outright. A `true`
/// answer means the next frame's layout pass hits the cache for every item and
/// costs a hash lookup per entry; a `false` answer means it would re-render
/// the whole history inline, which is what freezes the UI on a large session.
///
/// Lives beside [`render_variant`] rather than in the `jinn-chat-log-view`
/// slice deliberately: a coverage answer is only meaningful if it computes the
/// same cache key the render pass computes, and the slice cannot see the
/// domain-side render variant. The two loops are therefore kept adjacent and
/// pinned together by tests.
pub fn is_session_measured(
    cache: &mut EntryLineCache,
    state: &AppState,
    session_id: &SessionId,
    content_width: u16,
) -> bool {
    // Probed as a side effect: a width the cache has not seen clears it, and
    // the first probe then misses. Skipping the probe would make a resize look
    // like a warm cache, and the next frame would then do the full inline pass
    // this function exists to avoid.
    let Some(session) = state.session.get(session_id) else {
        return false;
    };
    let probe = CoverageProbe::new(state, session, content_width);
    probe.all_cached(cache)
}

/// The per-session inputs a coverage check resolves, gathered once so the walk
/// below reads as a single pass over the visual items.
struct CoverageProbe<'a> {
    history: &'a [ChatEntry],
    tool_result_statuses: HashMap<String, ToolResultStatus>,
    streaming_tool_call_ids: HashSet<ChatEntryId>,
    running_children: HashSet<SessionId>,
    expanded: HashSet<ChatEntryId>,
    shown_ignored_blocks: HashSet<ChatEntryId>,
    content_width: u16,
    min_collapse_count: usize,
}

impl<'a> CoverageProbe<'a> {
    /// Resolves the same inputs [`HistoryRender::compute_line_ranges`] resolves.
    ///
    fn new(state: &'a AppState, session: &'a ChatSessionState, content_width: u16) -> Self {
        Self {
            history: session.history(),
            tool_result_statuses: tool_result_statuses_of(session.history()),
            streaming_tool_call_ids: session.streaming_tool_call_ids(),
            running_children: running_session_ids(state),
            expanded: session.expanded_entry_ids(),
            shown_ignored_blocks: session.shown_ignored_blocks_snapshot(),
            content_width,
            // Read back from the last render rather than from the
            // configuration layer: this runs off the render thread, where no
            // config handle is in scope, and a threshold that disagreed with
            // the frame's would make the probe count items that frame will
            // never build. `None` before the first render, which is also the
            // built-in default.
            min_collapse_count: session
                .min_collapse_count()
                .unwrap_or(DEFAULT_MIN_COLLAPSE_COUNT),
        }
    }

    /// Whether every visual item resolves to a cache hit at the probed width.
    ///
    /// One `for` loop: the walk is the whole check.
    fn all_cached(&self, cache: &mut EntryLineCache) -> bool {
        let visual_items = build_visual_items(
            self.history,
            &self.shown_ignored_blocks,
            PROXIMITY_COUNT,
            self.min_collapse_count,
        );
        visual_items.iter().all(|item| match item {
            // A collapsed block is always exactly one line and is never stored
            // in the cache, so probing it would report a miss that no
            // measurement could ever fix.
            VisualItem::CollapsedIgnoredBlock { .. } => true,
            VisualItem::Entry(history_index) => {
                let Some(entry) = self.history.get(*history_index) else {
                    return false;
                };
                cache
                    .probe(
                        entry,
                        self.expanded.contains(&entry.id),
                        self.variant_of(entry),
                        self.content_width,
                    )
                    .hit
                    .is_some()
            }
        })
    }

    /// The render-variant key this entry would be probed under, computed
    /// exactly as the render pass and the layout worker compute it.
    fn variant_of(&self, entry: &ChatEntry) -> u64 {
        render_variant(
            paired_status_for(entry, &self.tool_result_statuses),
            is_streaming_tool_call(entry, &self.streaming_tool_call_ids),
            is_task_waiting(entry, &self.tool_result_statuses, &self.running_children),
        )
    }
}

/// Pairs tool call IDs with their result status for background coloring.
fn tool_result_statuses_of(history: &[ChatEntry]) -> HashMap<String, ToolResultStatus> {
    history
        .iter()
        .filter_map(|entry| match &entry.kind {
            ChatEntryKind::ToolResult { id, status, .. } => Some((id.clone(), *status)),
            _ => None,
        })
        .collect()
}

/// The ids of every session that is loaded and actively running.
fn running_session_ids(state: &AppState) -> HashSet<SessionId> {
    state
        .session
        .iter()
        .filter(|(_, child)| matches!(child.phase(), PhaseKind::Sending | PhaseKind::Streaming))
        .map(|(id, _)| id.clone())
        .collect()
}

/// The paired tool result status for an entry, if it has one.
fn paired_status_for(
    entry: &ChatEntry,
    tool_result_statuses: &HashMap<String, ToolResultStatus>,
) -> Option<ToolResultStatus> {
    match &entry.kind {
        ChatEntryKind::ToolCall { id, .. } => tool_result_statuses.get(id).copied(),
        ChatEntryKind::ToolResult { status, .. } => Some(*status),
        _ => None,
    }
}

/// Whether `entry` is a `ToolCall` whose arguments render through the
/// streaming path.
///
/// True for a call the phase machine still reports as streaming, and for one
/// that was abandoned mid-arguments: streamed, never finished, and forced out
/// of context. A turn that is interrupted or retried after a stall clears the
/// live streaming set but leaves the partial entry in history, and an
/// abandoned call is exactly as unfinished as a live one — collapsing it would
/// truncate arguments the user still needs to read.
///
/// The abandoned test requires `Streamed` timing: an `Instant`-timed call has
/// no finish stamp either, but it was never streaming to begin with.
fn is_streaming_tool_call(entry: &ChatEntry, streaming: &HashSet<ChatEntryId>) -> bool {
    if !matches!(&entry.kind, ChatEntryKind::ToolCall { .. }) {
        return false;
    }
    if streaming.contains(&entry.id) {
        return true;
    }
    matches!(entry.timing, EntryTiming::Streamed { .. })
        && entry.timing.finished_at().is_none()
        && !entry.is_in_context()
}

/// Whether this `task` call is still awaiting its result while its linked child
/// session is loaded and actively running.
fn is_task_waiting(
    entry: &ChatEntry,
    tool_result_statuses: &HashMap<String, ToolResultStatus>,
    running_children: &HashSet<SessionId>,
) -> bool {
    let ChatEntryKind::ToolCall {
        id,
        name,
        child_session,
        ..
    } = &entry.kind
    else {
        return false;
    };
    if name != TASK_TOOL_NAME {
        return false;
    }
    // A paired result means the tool already finished.
    if tool_result_statuses.contains_key(id) {
        return false;
    }
    child_session
        .as_ref()
        .is_some_and(|child| running_children.contains(child))
}

// ---------------------------------------------------------------------------
// History render pipeline
// ---------------------------------------------------------------------------

/// Accumulates state across the two-pass render pipeline.
///
/// The render pipeline is:
/// 1. `build_tool_result_map` - pair tool calls with their result status
/// 2. `compute_line_ranges` - cache-aware entry line counting (pass 1)
/// 3. `compute_scroll` - blank count, max offset, clamp, scroll-to-selected
/// 4. `find_visible_indices` - determine which entries overlap the viewport
/// 5. `build_blank_lines` - push blank spacer lines above content
/// 6. `render_visible_entries` - build content + gutter lines for visible entries (pass 2)
/// 7. `paint` - render the final paragraph widgets to the frame
struct HistoryRender<'a> {
    // Inputs (set once at construction)
    history: &'a [ChatEntry],
    visual_items: Vec<VisualItem>,
    selected_idx: Option<usize>,
    state: &'a AppState,
    /// This frame's chat-log settings, read once from the configuration
    /// layer. Resolving once per frame rather than per entry keeps the
    /// three call sites below consistent with each other even if a
    /// `reload` lands mid-frame.
    config: ChatLogConfig,
    content_width: u16,
    theme: Theme,
    area: Rect,
    gutter_area: Rect,
    content_area: Rect,

    // Built by pipeline steps
    /// The collapse threshold `compute_visual_items` resolved, published to
    /// the session so the off-thread coverage probe can match it.
    min_collapse_count: usize,
    tool_result_statuses: HashMap<String, ToolResultStatus>,
    /// Ids of the `ToolCall` entries streaming arguments right now.
    ///
    /// Snapshotted once per frame so layout can test membership per entry instead of
    /// scanning the whole history for each tool call.
    streaming_tool_call_ids: HashSet<ChatEntryId>,
    /// Child sessions loaded and actively running, by session id.
    ///
    /// Snapshotted for the same reason as the streaming set: the render pass
    /// tests membership per tool call, and scanning the session map inside that
    /// test would be O(entries x sessions) on every frame.
    running_children: HashSet<SessionId>,
    /// Per-visual-item wrapped line ranges: `entry_line_ranges[vi_idx] = (start, end)`.
    entry_line_ranges: Vec<(u32, u32)>,
    miss_lines: HashMap<usize, Vec<Line<'static>>>,
    #[expect(
        clippy::rc_buffer,
        reason = "Arc keeps cloning a rendered entry's line buffer O(1) where a plain Vec would deep-copy every line on each cache hit"
    )]
    cached_lines: HashMap<usize, Arc<Vec<Line<'static>>>>,
    total_wrapped: u32,
    scroll: ScrollState,
    visible_indices: Vec<usize>,
    content_lines: Vec<Line<'static>>,
    gutter_lines: Vec<Line<'static>>,
    lines_before_viewport: u32,
    /// The streaming entry's rendering, set only on a frame that actually
    /// rendered it. Read back by [`Self::stream_rendered`] so the caller can
    /// carry it into the next frame.
    stream_rendered: Option<(u32, Arc<Vec<Line<'static>>>)>,
}

impl<'a> HistoryRender<'a> {
    fn new(state: &'a AppState, area: Rect, config: &jinn_config::ConfigLayer) -> Self {
        let gutter_area = Rect {
            x: area.x,
            y: area.y,
            width: GUTTER_WIDTH,
            height: area.height,
        };
        let content_area = Rect {
            x: area.x + GUTTER_WIDTH,
            y: area.y,
            width: area.width.saturating_sub(GUTTER_WIDTH),
            height: area.height,
        };
        let streaming_tool_call_ids = state.active_session().streaming_tool_call_ids();
        let running_children = running_session_ids(state);
        Self {
            history: state.active_session().history(),
            selected_idx: state.active_session().selected_entry_index(),
            state,
            config: config.read::<ChatLogConfig>(),
            content_width: content_area.width,
            min_collapse_count: DEFAULT_MIN_COLLAPSE_COUNT,
            theme: state.frontend.theme.clone(),
            area,
            gutter_area,
            content_area,
            tool_result_statuses: HashMap::new(),
            streaming_tool_call_ids,
            running_children,
            entry_line_ranges: Vec::new(),
            miss_lines: HashMap::new(),
            cached_lines: HashMap::new(),
            total_wrapped: 0,
            scroll: ScrollState {
                blank_count: 0,
                max_offset: 0,
                clamped: 0,
            },
            visible_indices: Vec::new(),
            content_lines: Vec::new(),
            gutter_lines: Vec::new(),
            lines_before_viewport: 0,
            visual_items: Vec::new(),
            stream_rendered: None,
        }
    }

    /// Compute visual items from flat history and store on session state.
    ///
    /// Must be called before `compute_line_ranges`. The computed list is
    /// published to the session's view state only when it differs from the
    /// stored one, so a frame over unchanged history does not copy the list
    /// back.
    fn compute_visual_items(&mut self) {
        let shown_ignored_blocks = {
            let session = self.state.active_session();
            session.shown_ignored_blocks_snapshot()
        };
        let min_collapse = self
            .config
            .min_collapse_count
            .unwrap_or(DEFAULT_MIN_COLLAPSE_COUNT);
        self.min_collapse_count = min_collapse;
        let visual_items = build_visual_items(
            self.history,
            &shown_ignored_blocks,
            PROXIMITY_COUNT,
            min_collapse,
        );
        self.state
            .active_session()
            .set_visual_items_if_changed(&visual_items);
        self.visual_items = visual_items;
    }

    // -----------------------------------------------------------------------
    // Step 1: Build tool result status map
    // -----------------------------------------------------------------------

    /// The id of the entry currently accumulating assistant content, if the
    /// session is streaming.
    ///
    /// This is the single entry whose content fingerprint changes on every
    /// streamed token. Keying the throttle on the entry id rather than a
    /// history index keeps it correct when the history shifts underneath a
    /// frame.
    fn streaming_entry_id(&self) -> Option<ChatEntryId> {
        let session = self.state.active_session();
        if !matches!(session.phase(), PhaseKind::Streaming) {
            return None;
        }
        let hist_idx = session.streaming_entry_index()?;
        self.history.get(hist_idx).map(|entry| entry.id.clone())
    }

    /// The streaming entry's rendering, if this frame actually rendered it.
    ///
    /// Consumed by the caller so it can carry the lines into the next frame.
    /// `None` on a frame that reused the previous rendering instead.
    fn stream_rendered(&mut self) -> Option<(u32, Arc<Vec<Line<'static>>>)> {
        self.stream_rendered.take()
    }

    /// Whether `entry` is a `ToolCall` still streaming arguments.
    fn is_streaming_tool_call(&self, entry: &ChatEntry) -> bool {
        is_streaming_tool_call(entry, &self.streaming_tool_call_ids)
    }

    /// Pair tool call IDs with their result status for background coloring.
    fn build_tool_result_map(&mut self) {
        self.tool_result_statuses = tool_result_statuses_of(self.history);
    }

    // -----------------------------------------------------------------------
    // Step 2: Pass 1 - compute entry line ranges
    // -----------------------------------------------------------------------

    /// Walk all entries, compute wrapped line counts (using cache where possible),
    /// and record the (start, end) wrapped-line range for each entry.
    ///
    /// On a cache hit with rendered lines, the lines are stored in `cached_lines`
    /// for reuse in Pass 2. On a miss, lines are rendered, stored in both the cache
    /// (via `insert_with_lines`) and `miss_lines`.
    ///
    /// `streaming_id` names the entry accumulating assistant content, and
    /// `throttled` carries the previous frame's rendering of it. When both are
    /// present, that entry is painted from the previous rendering instead of
    /// being re-rendered: its line range is still recorded, so scroll state,
    /// the minimap, and the scrollbar stay in step with the entries around it.
    #[expect(clippy::expect_used, reason = "infallible")]
    fn compute_line_ranges(
        &mut self,
        cache: &mut EntryLineCache,
        streaming_id: Option<&ChatEntryId>,
        throttled: Option<&(u32, Arc<Vec<Line<'static>>>)>,
    ) {
        let mut wrapped_cursor: u32 = 0;

        for (vi_idx, item) in self.visual_items.iter().enumerate() {
            match item {
                VisualItem::Entry(hist_idx) => {
                    let entry = self
                        .history
                        .get(*hist_idx)
                        .expect("hist_idx from visual_items");
                    let is_expanded = self.state.active_session().is_entry_expanded(&entry.id);

                    // Paint the streaming entry from the previous frame rather
                    // than re-rendering it. Checked before the cache probe
                    // because the probe would miss anyway: the content really
                    // did change, which is the whole reason for throttling.
                    if let (Some(throttle_id), Some((wrapped_count, lines))) =
                        (streaming_id, throttled)
                    {
                        if entry.id == *throttle_id {
                            let start = wrapped_cursor;
                            let end = wrapped_cursor + wrapped_count;
                            self.entry_line_ranges.push((start, end));
                            wrapped_cursor = end;
                            self.cached_lines.insert(vi_idx, Arc::clone(lines));
                            continue;
                        }
                    }

                    // Variant hash covers status-derived look inputs; a
                    // changed variant forces a re-render even when the
                    // entry's content fingerprint is unchanged.
                    let variant = render_variant(
                        self.paired_status_for_entry(entry),
                        self.is_streaming_tool_call(entry),
                        self.is_task_waiting(entry),
                    );
                    let probe = cache.probe(entry, is_expanded, variant, self.content_width);
                    if let Some(hit) = probe.hit {
                        let start = wrapped_cursor;
                        let end = wrapped_cursor + hit.wrapped_count;
                        self.entry_line_ranges.push((start, end));
                        wrapped_cursor = end;
                        if let Some(lines) = hit.lines {
                            self.cached_lines.insert(vi_idx, lines);
                        }
                    } else {
                        let is_selected = self.selected_idx == Some(vi_idx);
                        let max_lines = self
                            .config
                            .tool_entry_max_lines
                            .unwrap_or(DEFAULT_TOOL_ENTRY_MAX_LINES);
                        let paired_status = self.paired_status_for_entry(entry);
                        let is_streaming = self.is_streaming_tool_call(entry);
                        let is_waiting_on_subagent = self.is_task_waiting(entry);
                        let variant =
                            render_variant(paired_status, is_streaming, is_waiting_on_subagent);
                        let ctx = RenderContext {
                            content_width: self.content_width,
                            is_selected,
                            is_expanded,
                            tool_entry_max_lines: max_lines,
                            theme: self.theme.clone(),
                            paired_status,
                            is_streaming,
                            is_waiting_on_subagent,
                        };
                        let lines = entry_to_lines(entry, &ctx);
                        let wrapped_count: u32 = if self.content_width == 0 {
                            lines.len() as u32
                        } else {
                            Paragraph::new(lines.clone())
                                .wrap(Wrap { trim: false })
                                .line_count(self.content_width) as u32
                        };

                        // Handed back to the caller so the next frame has
                        // something to reuse if this one is throttled.
                        if streaming_id.is_some_and(|id| *id == entry.id) {
                            self.stream_rendered = Some((wrapped_count, Arc::new(lines.clone())));
                        }

                        cache.insert_with_lines(
                            entry,
                            probe.content,
                            is_expanded,
                            variant,
                            self.content_width,
                            wrapped_count,
                            Arc::new(lines.clone()),
                        );

                        let start = wrapped_cursor;
                        let end = wrapped_cursor + wrapped_count;
                        self.entry_line_ranges.push((start, end));
                        wrapped_cursor = end;

                        self.miss_lines.insert(vi_idx, lines);
                    }
                }
                VisualItem::CollapsedIgnoredBlock { .. } => {
                    // Collapsed block is exactly 1 line.
                    let start = wrapped_cursor;
                    let end = wrapped_cursor + 1;
                    self.entry_line_ranges.push((start, end));
                    wrapped_cursor = end;
                }
            }
        }

        self.total_wrapped = wrapped_cursor;
        cache.evict_if_needed();
    }

    /// Look up the paired tool result status for an entry (if applicable).
    fn paired_status_for_entry(&self, entry: &ChatEntry) -> Option<ToolResultStatus> {
        paired_status_for(entry, &self.tool_result_statuses)
    }

    /// Whether this entry is a `task` tool call still awaiting its result
    /// while its linked child session is loaded in memory and actively
    /// running (sending or streaming).
    ///
    /// Drives the "Waiting for subagent session to complete" render line.
    fn is_task_waiting(&self, entry: &ChatEntry) -> bool {
        is_task_waiting(entry, &self.tool_result_statuses, &self.running_children)
    }

    // -----------------------------------------------------------------------
    // Step 3: Scroll math (delegates to the chat-log-view slice)
    // -----------------------------------------------------------------------

    fn compute_scroll(&mut self) {
        self.scroll = compute_scroll(
            self.area.height,
            self.total_wrapped,
            self.selected_idx,
            &self.entry_line_ranges,
            self.state.active_session().scroll_offset(),
        );
    }

    // -----------------------------------------------------------------------
    // Step 4: Find visible entries (delegates to the chat-log-view slice)
    // -----------------------------------------------------------------------

    fn find_visible_indices(&mut self) {
        self.visible_indices = find_visible_indices(
            &self.entry_line_ranges,
            self.scroll.blank_count,
            self.scroll.clamped,
            self.area.height,
        );
    }

    // -----------------------------------------------------------------------
    // Step 5: Blank lines above content
    // -----------------------------------------------------------------------

    /// Push blank spacer lines above the content when history is shorter than viewport.
    fn build_blank_lines(&mut self) {
        let blank_count = self.scroll.blank_count;
        let viewport_top = self.scroll.clamped;

        if blank_count > 0 && viewport_top < blank_count as u32 {
            for _ in 0..blank_count {
                self.content_lines.push(Line::from(""));
            }
            self.gutter_lines.extend(build_blank_gutter_lines(
                blank_count,
                &self.theme,
                GUTTER_STR,
            ));
            self.lines_before_viewport = viewport_top;
        }
    }

    // -----------------------------------------------------------------------
    // Step 6: Pass 2 - render visible entries
    // -----------------------------------------------------------------------

    /// Store freshly painted lines for an entry and mark them as recently used.
    ///
    /// The wrapped count is read back from the range Pass 1 computed, so the
    /// cache never disagrees with the layout that was just used to paint.
    fn cache_lines(
        &self,
        cache: &mut EntryLineCache,
        entry: &ChatEntry,
        vi_idx: usize,
        is_expanded: bool,
        variant: u64,
        lines: Vec<Line<'static>>,
    ) {
        let wrapped_count = self
            .entry_line_ranges
            .get(vi_idx)
            .map_or(0, |(start, end)| end - start);
        let content = cache
            .probe(entry, is_expanded, variant, self.content_width)
            .content;
        cache.insert_with_lines(
            entry,
            content,
            is_expanded,
            variant,
            self.content_width,
            wrapped_count,
            Arc::new(lines),
        );
        cache.touch(&entry.id);
    }

    /// Build content and gutter lines for all visible entries.
    #[expect(clippy::expect_used, reason = "infallible")]
    fn render_visible_entries(&mut self, cache: &mut EntryLineCache) {
        let viewport_top = self.scroll.clamped;
        let chat_log_active =
            matches!(self.state.frontend.scope(), jinn_slices::FocusScope::Normal);
        let cursor_color = self.theme.focus_accent;

        for &vi_idx in &self.visible_indices {
            let (entry_start, entry_end) = self
                .entry_line_ranges
                .get(vi_idx)
                .copied()
                .expect("vi_idx from visible_indices");
            let abs_entry_start = entry_start + self.scroll.blank_count as u32;

            match self.visual_items.get(vi_idx) {
                Some(VisualItem::Entry(hist_idx)) => {
                    let entry = self
                        .history
                        .get(*hist_idx)
                        .expect("hist_idx from visual_items");
                    let is_selected = self.selected_idx == Some(vi_idx);
                    let is_expanded = self.state.active_session().is_entry_expanded(&entry.id);
                    let max_lines = self
                        .config
                        .tool_entry_max_lines
                        .unwrap_or(DEFAULT_TOOL_ENTRY_MAX_LINES);
                    let variant = render_variant(
                        self.paired_status_for_entry(entry),
                        self.is_streaming_tool_call(entry),
                        self.is_task_waiting(entry),
                    );

                    // Get content lines - cached lines → miss lines → render fresh.
                    let entry_content_lines = if let Some(lines) = self.cached_lines.remove(&vi_idx)
                    {
                        // Painted from the cache, so this entry counts as used.
                        cache.touch(&entry.id);
                        Arc::unwrap_or_clone(lines)
                    } else if let Some(lines) = self.miss_lines.remove(&vi_idx) {
                        // Freshly rendered during layout this frame; store it so
                        // a scroll away and back can reuse it.
                        self.cache_lines(cache, entry, vi_idx, is_expanded, variant, lines.clone());
                        lines
                    } else {
                        // Nothing available: render, then cache for the next frame.
                        let paired_status = self.paired_status_for_entry(entry);
                        let is_streaming = self.is_streaming_tool_call(entry);
                        let is_waiting_on_subagent = self.is_task_waiting(entry);
                        let ctx = RenderContext {
                            content_width: self.content_width,
                            is_selected,
                            is_expanded,
                            tool_entry_max_lines: max_lines,
                            theme: self.theme.clone(),
                            paired_status,
                            is_streaming,
                            is_waiting_on_subagent,
                        };
                        let lines = entry_to_lines(entry, &ctx);
                        self.cache_lines(cache, entry, vi_idx, is_expanded, variant, lines.clone());
                        lines
                    };

                    // Build gutter lines for this entry.
                    let is_pinned = entry.pin_position.is_some();
                    let is_included_in_context = entry.is_in_context();
                    let gutter_ctx = GutterStyle {
                        is_pinned,
                        is_selected,
                        chat_log_active,
                        content_width: self.content_width,
                        // Pass 1 already measured how many rows this entry
                        // wraps to at this width.
                        wrapped_count: entry_end - entry_start,
                        theme: &self.theme,
                        cursor_color,
                        is_included_in_context,
                        gutter_context_color: self.theme.gutter_context_included,
                    };
                    let entry_gutter_lines =
                        build_entry_gutter_lines(&entry_content_lines, &gutter_ctx);

                    // Track lines above viewport for scroll calculation.
                    if abs_entry_start < viewport_top {
                        self.lines_before_viewport += viewport_top.saturating_sub(abs_entry_start);
                    }

                    self.content_lines.extend(entry_content_lines);
                    self.gutter_lines.extend(entry_gutter_lines);
                }
                Some(VisualItem::CollapsedIgnoredBlock { count, .. }) => {
                    let is_selected = self.selected_idx == Some(vi_idx);

                    // Content: gray summary line.
                    let text = format!("{count} hidden entries (press h to show)");
                    let style = Style::default().fg(self.theme.border_unfocused);
                    let line = Line::from(Span::styled(text, style));
                    self.content_lines.push(line);

                    // Gutter: gray indicator with optional cursor.
                    let gutter_line = build_collapsed_block_gutter_line(
                        is_selected,
                        chat_log_active,
                        &self.theme,
                        cursor_color,
                    );
                    self.gutter_lines.push(gutter_line);

                    // Track lines above viewport.
                    if abs_entry_start < viewport_top {
                        self.lines_before_viewport += viewport_top.saturating_sub(abs_entry_start);
                    }
                }
                None => {}
            }
        }
    }

    // -----------------------------------------------------------------------
    // Step 7: Paint
    // -----------------------------------------------------------------------

    /// Render the final gutter and content paragraph widgets to the frame.
    fn paint(self, frame: &mut Frame<'_>) {
        // ratatui's `Paragraph::scroll` takes u16, so the u32 line math is
        // narrowed at this boundary. A session long enough to overflow u16 rows
        // cannot be scrolled to in one frame anyway.
        let paragraph_scroll = u16::try_from(self.lines_before_viewport).unwrap_or(u16::MAX);

        // Render gutter column.
        let gutter_widget = Paragraph::new(self.gutter_lines)
            .block(Block::default().borders(Borders::NONE))
            .scroll((paragraph_scroll, 0));
        frame.render_widget(gutter_widget, self.gutter_area);

        // Render content column.
        let chat_widget = Paragraph::new(self.content_lines)
            .block(Block::default().borders(Borders::NONE))
            .wrap(Wrap { trim: false })
            .scroll((paragraph_scroll, 0));
        frame.render_widget(chat_widget, self.content_area);

        // Render scroll indicator (delegates to the chat-log-view slice).
        // The indicator is a u16 widget; clamping both values preserves the
        // `clamped >= max_offset` "at the bottom" check it relies on.
        render_scroll_indicator(
            frame,
            self.area,
            u16::try_from(self.scroll.clamped).unwrap_or(u16::MAX),
            u16::try_from(self.scroll.max_offset).unwrap_or(u16::MAX),
            &self.theme,
        );
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, reason = "test code")]

    use jinn_core_types::EntryTiming;
    use jinn_core_types::context_override::ContextOverride;

    use super::{ChatEntry, is_streaming_tool_call};
    use std::collections::HashSet;

    /// An abandoned partial tool call: streamed, never finished, out of context.
    fn abandoned_partial() -> ChatEntry {
        let mut entry = ChatEntry::tool_call("call-1", "edit", "{\"file_path\":\"src/a.ts\"");
        entry.timing = EntryTiming::streamed(jiff::Timestamp::now());
        entry.context_override = ContextOverride::ForcedExclude;
        entry
    }

    #[rstest::rstest]
    #[test]
    fn abandoned_partial_tool_call_renders_through_the_streaming_path() {
        // Given a tool call that was interrupted mid-arguments and then excluded.
        let entry = abandoned_partial();

        // When testing whether its arguments render streaming.
        let streaming = is_streaming_tool_call(&entry, &HashSet::new());

        // Then it streams, so the arguments are not collapsed.
        assert!(streaming);
    }

    #[rstest::rstest]
    #[test]
    fn finalized_tool_call_renders_collapsed() {
        // Given a tool call that ran to completion.
        let mut entry = abandoned_partial();
        entry.timing.finish();
        entry.context_override = ContextOverride::Default;

        // When testing whether its arguments render streaming.
        let streaming = is_streaming_tool_call(&entry, &HashSet::new());

        // Then it does not stream, so it collapses.
        assert!(!streaming);
    }

    #[rstest::rstest]
    #[test]
    fn live_partial_tool_call_renders_through_the_streaming_path() {
        // Given a tool call the phase machine still reports as streaming.
        let mut entry = abandoned_partial();
        entry.context_override = ContextOverride::Default;
        let streaming = HashSet::from([entry.id.clone()]);

        // When testing whether its arguments render streaming.
        let is_streaming = is_streaming_tool_call(&entry, &streaming);

        // Then it streams on the strength of the live set alone.
        assert!(is_streaming);
    }

    #[rstest::rstest]
    #[test]
    fn pinned_abandoned_partial_tool_call_renders_collapsed() {
        // Given an interrupted tool call that was pinned back into context.
        let mut entry = abandoned_partial();
        entry.context_override = ContextOverride::ForcedInclude;

        // When testing whether its arguments render streaming.
        let streaming = is_streaming_tool_call(&entry, &HashSet::new());

        // Then it does not stream, because a pinned entry is not abandoned.
        assert!(!streaming);
    }

    #[rstest::rstest]
    #[test]
    fn instant_timed_excluded_tool_call_renders_collapsed() {
        // Given an excluded tool call that never went through a streaming lifecycle.
        let mut entry = ChatEntry::tool_call("call-1", "edit", "{}");
        entry.context_override = ContextOverride::ForcedExclude;

        // When testing whether its arguments render streaming.
        let streaming = is_streaming_tool_call(&entry, &HashSet::new());

        // Then it does not stream: it has no finish stamp but never streamed either.
        assert!(!streaming);
    }

    #[rstest::rstest]
    #[test]
    fn abandoned_partial_user_entry_renders_collapsed() {
        // Given an excluded user message carrying streamed timing.
        let mut entry = ChatEntry::user("hello");
        entry.timing = EntryTiming::streamed(jiff::Timestamp::now());
        entry.context_override = ContextOverride::ForcedExclude;

        // When testing whether it renders streaming.
        let streaming = is_streaming_tool_call(&entry, &HashSet::new());

        // Then the streaming path is tool-call-only, so this message does not take it.
        assert!(!streaming);
    }
}
