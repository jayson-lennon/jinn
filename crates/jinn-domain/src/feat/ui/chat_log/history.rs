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

mod gutter;
mod scroll_indicator;
mod viewport;

use std::collections::HashMap;
use std::sync::Arc;

use crate::common::app_state::AppState;
use crate::common::render_ctx::RenderCtx;
use crate::common::ui_element::UiElement;
use crate::feat::session::phase_machine::PhaseKind;
use crate::feat::session::tool_result_status::ToolResultStatus;
use crate::feat::theme::Theme;
use crate::protocol::{ChatEntry, ChatEntryKind};
use jinn_tools_msg::TASK_TOOL_NAME;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use super::line_count_cache::EntryLineCache;
use super::shared::{GUTTER_WIDTH, RenderContext};
use super::visual_item::{
    DEFAULT_MIN_COLLAPSE_COUNT, PROXIMITY_COUNT, VisualItem, build_visual_items,
};
use super::{
    actor, annotation, assistant, compaction, error_entry, system, thinking, tool_call,
    tool_result, transient, user,
};
use viewport::ScrollState;

/// Default number of lines to show for tool entries (calls and results) before truncating.
const DEFAULT_TOOL_ENTRY_MAX_LINES: u16 = 6;
// alternatives: |❚┃╏⣿𜺏░▒▓
const GUTTER_STR: &str = "𜺏 ";

/// Hash of the status-derived render inputs that change an entry's rendered
/// lines without changing its content fingerprint (paired tool-result
/// background tint, streaming flag, subagent-waiting line). Used as the
/// render-variant component of the entry line cache key so the cache
/// invalidates when any of them flips.
fn render_variant(
    paired_status: Option<ToolResultStatus>,
    is_streaming: bool,
    is_waiting_on_subagent: bool,
) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (paired_status, is_streaming, is_waiting_on_subagent).hash(&mut hasher);
    hasher.finish()
}

/// Display element for the full conversation history.
#[derive(Debug, Default)]
pub struct ChatLogElement;

impl ChatLogElement {
    /// Create a new chat log element.
    #[must_use]
    pub fn new() -> Self {
        Self
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
        let state = ctx.state;
        if state.session.is_loading() {
            render_loading(frame, area, &state.frontend.theme);
            return;
        }

        let mut render = HistoryRender::new(state, area);
        render.compute_visual_items();
        render.build_tool_result_map();
        {
            let mut cache = state.frontend.caches.entry_line_cache.write();
            render.compute_line_ranges(&mut cache);
        }
        render.compute_scroll();

        {
            let session = state.active_session();
            session.set_last_max_offset(render.scroll.max_offset);
            session.set_entry_line_ranges(render.entry_line_ranges.clone());
            session.set_viewport_height(area.height);
            session.set_blank_count(render.scroll.blank_count as u16);
            session.set_rendered_scroll_offset(render.scroll.clamped);
        }

        render.find_visible_indices();
        render.build_blank_lines();
        render.render_visible_entries();
        render.paint(frame);
    }
}

// ---------------------------------------------------------------------------
// Loading indicator
// ---------------------------------------------------------------------------

/// Render a centered "Loading session..." message while a session loads.
fn render_loading(frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
    let loading = Paragraph::new("Loading session...")
        .alignment(ratatui::layout::Alignment::Center)
        .style(Style::default().fg(theme.muted_text))
        .block(Block::default().borders(Borders::NONE));
    frame.render_widget(loading, area);
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
    content_width: u16,
    theme: Theme,
    area: Rect,
    gutter_area: Rect,
    content_area: Rect,

    // Built by pipeline steps
    tool_result_statuses: HashMap<String, ToolResultStatus>,
    /// Per-visual-item wrapped line ranges: `entry_line_ranges[vi_idx] = (start, end)`.
    entry_line_ranges: Vec<(u16, u16)>,
    miss_lines: HashMap<usize, Vec<Line<'static>>>,
    #[expect(
        clippy::rc_buffer,
        reason = "Vec<Line> not Send, Arc used for cheap clone within same thread"
    )]
    cached_lines: HashMap<usize, Arc<Vec<Line<'static>>>>,
    total_wrapped: u16,
    scroll: ScrollState,
    visible_indices: Vec<usize>,
    content_lines: Vec<Line<'static>>,
    gutter_lines: Vec<Line<'static>>,
    lines_before_viewport: u16,
}

impl<'a> HistoryRender<'a> {
    fn new(state: &'a AppState, area: Rect) -> Self {
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
        Self {
            history: state.active_session().history(),
            selected_idx: state.active_session().selected_entry_index(),
            state,
            content_width: content_area.width,
            theme: state.frontend.theme.clone(),
            area,
            gutter_area,
            content_area,
            tool_result_statuses: HashMap::new(),
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
        }
    }

    /// Compute visual items from flat history and store on session state.
    ///
    /// Must be called before `compute_line_ranges`.
    fn compute_visual_items(&mut self) {
        let session = self.state.active_session();
        let shown_ignored_blocks = session.shown_ignored_blocks_snapshot();
        let min_collapse = self
            .state
            .frontend
            .preferences
            .min_collapse_count
            .unwrap_or(DEFAULT_MIN_COLLAPSE_COUNT);
        let visual_items = build_visual_items(
            self.history,
            &shown_ignored_blocks,
            PROXIMITY_COUNT,
            min_collapse,
        );
        self.state
            .active_session()
            .set_visual_items(visual_items.clone());
        self.visual_items = visual_items;
    }

    // -----------------------------------------------------------------------
    // Step 1: Build tool result status map
    // -----------------------------------------------------------------------

    /// Pair tool call IDs with their result status for background coloring.
    fn build_tool_result_map(&mut self) {
        self.tool_result_statuses = self
            .history
            .iter()
            .filter_map(|entry| match &entry.kind {
                ChatEntryKind::ToolResult { id, status, .. } => Some((id.clone(), *status)),
                _ => None,
            })
            .collect();
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
    #[expect(clippy::expect_used, reason = "infallible")]
    fn compute_line_ranges(&mut self, cache: &mut EntryLineCache) {
        let mut wrapped_cursor: u16 = 0;

        for (vi_idx, item) in self.visual_items.iter().enumerate() {
            match item {
                VisualItem::Entry(hist_idx) => {
                    let entry = self
                        .history
                        .get(*hist_idx)
                        .expect("hist_idx from visual_items");
                    let is_expanded = self.state.active_session().is_entry_expanded(&entry.id);

                    // Variant hash covers status-derived look inputs; a
                    // changed variant forces a re-render even when the
                    // entry's content fingerprint is unchanged.
                    let variant = render_variant(
                        self.paired_status_for_entry(entry),
                        matches!(&entry.kind, ChatEntryKind::ToolCall { .. })
                            && self
                                .state
                                .active_session()
                                .is_tool_call_streaming(&entry.id),
                        self.is_task_waiting(entry),
                    );
                    if let Some(hit) = cache.get(entry, is_expanded, variant, self.content_width) {
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
                            .state
                            .frontend
                            .preferences
                            .tool_entry_max_lines
                            .unwrap_or(DEFAULT_TOOL_ENTRY_MAX_LINES);
                        let paired_status = self.paired_status_for_entry(entry);
                        let is_streaming = matches!(&entry.kind, ChatEntryKind::ToolCall { .. })
                            && self
                                .state
                                .active_session()
                                .is_tool_call_streaming(&entry.id);
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
                        let wrapped_count: u16 = if self.content_width == 0 {
                            lines.len() as u16
                        } else {
                            Paragraph::new(lines.clone())
                                .wrap(Wrap { trim: false })
                                .line_count(self.content_width) as u16
                        };
                        cache.insert_with_lines(
                            entry,
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
    }

    /// Look up the paired tool result status for an entry (if applicable).
    fn paired_status_for_entry(&self, entry: &ChatEntry) -> Option<ToolResultStatus> {
        match &entry.kind {
            ChatEntryKind::ToolCall { id, .. } => self.tool_result_statuses.get(id).copied(),
            ChatEntryKind::ToolResult { status, .. } => Some(*status),
            _ => None,
        }
    }

    /// Whether this entry is a `task` tool call still awaiting its result
    /// while its linked child session is loaded in memory and actively
    /// running (sending or streaming).
    ///
    /// Drives the "Waiting for subagent session to complete" render line.
    fn is_task_waiting(&self, entry: &ChatEntry) -> bool {
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
        // No paired result yet: a pending entry exists only before the tool
        // starts executing, so any status here means the result has landed.
        if self.tool_result_statuses.contains_key(id) {
            return false;
        }
        let Some(child_id) = child_session else {
            return false;
        };
        let Some(child) = self.state.session.get(child_id) else {
            return false;
        };
        matches!(child.phase(), PhaseKind::Sending | PhaseKind::Streaming)
    }

    // -----------------------------------------------------------------------
    // Step 3: Scroll math (delegates to viewport submodule)
    // -----------------------------------------------------------------------

    fn compute_scroll(&mut self) {
        self.scroll = viewport::compute_scroll(
            self.area.height,
            self.total_wrapped,
            self.selected_idx,
            &self.entry_line_ranges,
            self.state.active_session().scroll_offset(),
        );
    }

    // -----------------------------------------------------------------------
    // Step 4: Find visible entries (delegates to viewport submodule)
    // -----------------------------------------------------------------------

    fn find_visible_indices(&mut self) {
        self.visible_indices = viewport::find_visible_indices(
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

        if blank_count > 0 && viewport_top < blank_count as u16 {
            for _ in 0..blank_count {
                self.content_lines.push(Line::from(""));
            }
            self.gutter_lines.extend(gutter::build_blank_gutter_lines(
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

    /// Build content and gutter lines for all visible entries.
    #[expect(clippy::expect_used, reason = "infallible")]
    fn render_visible_entries(&mut self) {
        let viewport_top = self.scroll.clamped;
        let chat_log_active = matches!(
            self.state.frontend.scope(),
            crate::common::app_state::FocusScope::Normal
        );
        let cursor_color = self.theme.focus_accent;

        for &vi_idx in &self.visible_indices {
            let (entry_start, _entry_end) = self
                .entry_line_ranges
                .get(vi_idx)
                .copied()
                .expect("vi_idx from visible_indices");
            let abs_entry_start = entry_start + self.scroll.blank_count as u16;

            match self.visual_items.get(vi_idx) {
                Some(VisualItem::Entry(hist_idx)) => {
                    let entry = self
                        .history
                        .get(*hist_idx)
                        .expect("hist_idx from visual_items");
                    let is_selected = self.selected_idx == Some(vi_idx);
                    let is_expanded = self.state.active_session().is_entry_expanded(&entry.id);
                    let max_lines = self
                        .state
                        .frontend
                        .preferences
                        .tool_entry_max_lines
                        .unwrap_or(DEFAULT_TOOL_ENTRY_MAX_LINES);

                    // Get content lines - cached lines → miss lines → render fresh.
                    let entry_content_lines = if let Some(lines) = self.cached_lines.remove(&vi_idx)
                    {
                        Arc::unwrap_or_clone(lines)
                    } else if let Some(lines) = self.miss_lines.remove(&vi_idx) {
                        lines
                    } else {
                        let paired_status = self.paired_status_for_entry(entry);
                        let is_streaming = matches!(&entry.kind, ChatEntryKind::ToolCall { .. })
                            && self
                                .state
                                .active_session()
                                .is_tool_call_streaming(&entry.id);
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
                        entry_to_lines(entry, &ctx)
                    };

                    // Build gutter lines for this entry.
                    let is_pinned = entry.pin_position.is_some();
                    let is_included_in_context = entry.is_in_context();
                    let gutter_ctx = gutter::GutterStyle {
                        is_pinned,
                        is_selected,
                        chat_log_active,
                        content_width: self.content_width,
                        theme: &self.theme,
                        cursor_color,
                        is_included_in_context,
                        gutter_context_color: self.theme.gutter_context_included,
                    };
                    let entry_gutter_lines =
                        gutter::build_entry_gutter_lines(&entry_content_lines, &gutter_ctx);

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
                    let gutter_line = gutter::build_collapsed_block_gutter_line(
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
        let paragraph_scroll = self.lines_before_viewport;

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

        // Render scroll indicator (delegates to scroll_indicator submodule).
        scroll_indicator::render_scroll_indicator(
            frame,
            self.area,
            self.scroll.clamped,
            self.scroll.max_offset,
            &self.theme,
        );
    }
}

// ---------------------------------------------------------------------------
// Entry dispatch
// ---------------------------------------------------------------------------

/// Convert a chat entry into one or more visual lines, splitting on `\n`.
///
/// Each entry type is delegated to its own submodule. Lines returned here are
/// content-width only - the gutter is rendered as a separate column.
pub fn entry_to_lines(entry: &ChatEntry, ctx: &RenderContext) -> Vec<Line<'static>> {
    match &entry.kind {
        ChatEntryKind::User {
            display, outcome, ..
        } => user::to_lines(display, outcome, ctx),
        ChatEntryKind::System(text) => system::to_lines(text, ctx),
        ChatEntryKind::Error(text) => error_entry::to_lines(text, ctx),
        ChatEntryKind::Actor { source, text } => actor::to_lines(source, text, ctx),
        ChatEntryKind::Assistant(text) => assistant::to_lines(text, ctx),
        ChatEntryKind::ToolCall {
            name, arguments, ..
        } => tool_call::to_lines(name, arguments, ctx),
        ChatEntryKind::ToolResult {
            name,
            content,
            status,
            truncation,
            is_alert,
            ..
        } => tool_result::to_lines(name, content, *status, truncation.as_ref(), *is_alert, ctx),
        ChatEntryKind::Thinking(text) => thinking::to_lines(text, ctx),
        ChatEntryKind::Annotation { citations } => annotation::to_lines(citations, ctx),

        ChatEntryKind::Transient(text) => transient::to_lines(text, ctx),
        ChatEntryKind::Compaction {
            summary,
            entries_compacted,
            tokens_before,
            tokens_after,
            ..
        } => compaction::to_lines(
            summary,
            *entries_compacted,
            *tokens_before,
            *tokens_after,
            ctx,
        ),
    }
}
