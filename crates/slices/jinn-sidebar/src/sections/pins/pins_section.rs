//! [`PinsSection`] - the pinned entries sidebar section.
//!
//! Implements [`SidebarSection`] for pinned context entries.
//! Also provides handler functions that the `IntentHandler` calls
//! for sidebar and pins intents.

use crate::sections::section_trait::{
    EnterFrom, SectionNavResult, SidebarIntent, SidebarSection, SidebarSectionId,
};
use jinn_domain::common::app_state::AppState;
use jinn_domain::common::app_state::pin_sort_key;
use jinn_domain::common::render_ctx::RenderCtx;
use jinn_domain::feat::context::protocol::command::{PinChatEntry, UnpinChatEntry};
use jinn_domain::feat::skills::loaded_skill_summary_label;
use jinn_domain::feat::theme::Theme;
use jinn_domain::feat::ui::chat_log::strip_ansi;
use jinn_domain::protocol::ToolResultStatus;
use jinn_domain::protocol::{
    ChatEntryId, ChatEntryKind, IntentResult, PickerKind, PinPosition, SessionId,
};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

/// The pinned entries sidebar section.
///
/// Renders pinned context entries with position badges and selection highlighting.
/// Handles navigation (up/down) within the pins list and delegates boundary
/// crossings to the sidebar container.
#[derive(Debug)]
pub struct PinsSection;

/// Navigate within the pins section.
///
/// Moves the cursor within the pins list, or returns `Exhausted` when
/// at a boundary or when the list is empty. Does NOT modify cursor state
/// on exhaustion - the sidebar decides what to do.
pub fn navigate(intent: &SidebarIntent, state: &mut AppState) -> SectionNavResult {
    let sorted_ids = state.sorted_pinned_ids();
    if sorted_ids.is_empty() {
        return SectionNavResult::Exhausted;
    }
    match intent {
        SidebarIntent::MoveDown => {
            let current = state
                .frontend
                .with_sections(|s| s.pins.selection_index(&sorted_ids), || 0);
            if current >= sorted_ids.len() - 1 {
                return SectionNavResult::Exhausted;
            }
            state
                .frontend
                .update_sections(|s| s.pins.select_next(&sorted_ids));
            sync_chat_log_cursor(state);
            SectionNavResult::Moved
        }
        SidebarIntent::MoveUp => {
            let current = state
                .frontend
                .with_sections(|s| s.pins.selection_index(&sorted_ids), || 0);
            if current == 0 {
                return SectionNavResult::Exhausted;
            }
            state
                .frontend
                .update_sections(|s| s.pins.select_prev(&sorted_ids));
            sync_chat_log_cursor(state);
            SectionNavResult::Moved
        }
        SidebarIntent::Action(_) => SectionNavResult::Moved,
    }
}

/// Place the cursor on this section from a given direction.
pub fn receive_cursor(state: &mut AppState, enter_from: EnterFrom) {
    // Save current history position before the pin cursor changes it.
    state.active_session_mut().save_history_position();

    let sorted_ids = state.sorted_pinned_ids();
    match enter_from {
        EnterFrom::Top => {
            if let Some(first) = sorted_ids.first() {
                state
                    .frontend
                    .update_sections(|s| s.pins.select_by_id(first.clone()));
            }
        }
        EnterFrom::Bottom => {
            if let Some(last) = sorted_ids.last() {
                state
                    .frontend
                    .update_sections(|s| s.pins.select_by_id(last.clone()));
            }
        }
    }
    sync_chat_log_cursor(state);
}

impl SidebarSection for PinsSection {
    fn id(&self) -> SidebarSectionId {
        jinn_slices::SidebarSectionId::Pins
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx) {
        let state = ctx.state;
        let sorted_ids = state.sorted_pinned_ids();
        let mut pinned = state.active_session().pinned_entries();
        // Sort to match sorted_ids order (TOP → REL → BOT, stable by history).
        pinned.sort_by_key(|entry| pin_sort_key(entry.pin_position));

        let selected_index = if state
            .frontend
            .with_sections(|s| s.pins.selected_id().is_some(), || false)
        {
            state
                .frontend
                .with_sections(|s| s.pins.selection_index(&sorted_ids), || 0)
        } else {
            usize::MAX // No pin will match this index.
        };
        let lines = if pinned.is_empty() {
            vec![Line::from(vec![Span::styled(
                " Pinned Context \u{2014} 0",
                Style::default()
                    .fg(state.frontend.theme.primary_text)
                    .add_modifier(Modifier::BOLD),
            )])]
        } else {
            let sidebar_focused = state.frontend.is_sidebar();
            let section_focused = sidebar_focused
                && matches!(
                    state.frontend.sidebar_section(),
                    Some(jinn_slices::SidebarSectionId::Pins)
                );
            build_entry_list(
                &pinned,
                selected_index,
                area.width,
                sidebar_focused,
                section_focused,
                &state.frontend.theme,
            )
        };

        let total_lines = lines.len() as u16;
        let max_offset = total_lines.saturating_sub(area.height);
        let scroll_offset = max_offset;

        let widget = Paragraph::new(lines)
            .block(Block::default().borders(Borders::NONE))
            .scroll((scroll_offset, 0));
        frame.render_widget(widget, area);
    }

    fn content_height(&self, ctx: &RenderCtx) -> u16 {
        let state = ctx.state;
        let count = state.active_session().pinned_entries().len();
        // Hide the section entirely when there are no pins.
        if count == 0 {
            return 0;
        }
        let count = count as u16;
        // Header(1) + header-gap(1) + entries(count) + trailing gap(1).
        count + 3
    }
}

/// Computes the pins section content height from state.
///
/// Mirrors [`PinsSection::content_height`] so the task list preview popup
/// can determine where the task list section starts without needing the
/// section instance. Hides (0) when there are no pinned entries.
///
/// [`PinsSection::content_height`]: PinsSection::content_height
#[must_use]
pub fn pins_section_content_height(state: &AppState) -> u16 {
    let count = state.active_session().pinned_entries().len();
    if count == 0 {
        return 0;
    }
    // Header(1) + header-gap(1) + entries(count) + trailing gap(1).
    count as u16 + 3
}

// ---------------------------------------------------------------------------
// Intent handler functions (called by IntentHandler)
// ---------------------------------------------------------------------------

/// Handles the persona edit key - opens the persona picker when the persona section is focused.
///
/// No-op if the pins section is focused.
pub fn handle_sidebar_persona_edit(
    state: &mut AppState,
    pickers: &jinn_picker::PickerRegistry,
) -> IntentResult {
    if !matches!(
        state.frontend.sidebar_section(),
        Some(jinn_slices::SidebarSectionId::Persona)
    ) {
        return IntentResult::empty();
    }
    jinn_domain::feat::picker::intent::handle_open_picker(state, PickerKind::Persona, pickers)
}

/// Handles `PinsUnpin`.
pub fn handle_pins_unpin(state: &mut AppState) -> IntentResult {
    if super::validator::validate_unpin(state).is_err() {
        return IntentResult::empty();
    }
    if let Some((session_id, entry_id)) = resolve_selected_entry_id(state) {
        IntentResult::new_message(UnpinChatEntry {
            session_id,
            entry_id,
        })
    } else {
        IntentResult::empty()
    }
}

/// Handles `PinsPinTop/Bottom/Relative`.
pub fn handle_pins_pin(state: &mut AppState, position: PinPosition) -> IntentResult {
    if super::validator::validate_pin(state).is_err() {
        return IntentResult::empty();
    }
    if let Some((session_id, entry_id)) = resolve_selected_entry_id(state) {
        IntentResult::new_message(PinChatEntry {
            session_id,
            entry_id,
            position,
        })
    } else {
        IntentResult::empty()
    }
}

/// Handles `PinsPinCycle`.
pub fn handle_pins_pin_cycle(state: &mut AppState) -> IntentResult {
    if super::validator::validate_pin_cycle(state).is_err() {
        return IntentResult::empty();
    }
    let sorted_ids = state.sorted_pinned_ids();
    let index = state
        .frontend
        .with_sections(|s| s.pins.selection_index(&sorted_ids), || 0);
    let mut pinned = state.active_session().pinned_entries();
    pinned.sort_by_key(|entry| pin_sort_key(entry.pin_position));
    let Some(entry) = pinned.get(index) else {
        return IntentResult::empty();
    };
    let current = entry.pin_position.unwrap_or(PinPosition::Relative);
    let next = cycle_position(current);
    let session_id = state.session.active_session_id().clone();
    let entry_id = entry.id.clone();
    IntentResult::new_message(PinChatEntry {
        session_id,
        entry_id,
        position: next,
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Sync the chat log cursor to the currently selected pinned entry.
///
/// When a pinned entry is selected in the sidebar, this sets the chat log's
/// `selected_entry_index` to the history index of that pinned entry so the
/// renderer scrolls to show it.
pub(crate) fn sync_chat_log_cursor(state: &mut AppState) {
    let Some(pinned_id) = state
        .frontend
        .with_sections(|s| s.pins.selected_id().cloned(), || None)
    else {
        return;
    };
    if state
        .active_session()
        .history()
        .iter()
        .any(|e| e.id == pinned_id)
    {
        state.active_session_mut().set_selected_cursor_id(pinned_id);
    }
}
fn resolve_selected_entry_id(state: &AppState) -> Option<(SessionId, ChatEntryId)> {
    let sorted_ids = state.sorted_pinned_ids();
    let index = state
        .frontend
        .with_sections(|s| s.pins.selection_index(&sorted_ids), || 0);
    let session_id = state.session.active_session_id().clone();

    let mut pinned = state.active_session().pinned_entries();
    pinned.sort_by_key(|entry| pin_sort_key(entry.pin_position));

    let entry = pinned.get(index)?;
    Some((session_id, entry.id.clone()))
}

/// Cycles a pin position to the next value in the rotation: Top → Bottom → Relative → Top.
fn cycle_position(pos: PinPosition) -> PinPosition {
    match pos {
        PinPosition::Top => PinPosition::Bottom,
        PinPosition::Bottom => PinPosition::Relative,
        PinPosition::Relative => PinPosition::Top,
    }
}

// ---------------------------------------------------------------------------
// Rendering helpers
// ---------------------------------------------------------------------------

/// Solid yellow full block used as the selection indicator.
const SELECTED_INDICATOR: &str = "\u{2588}";
/// One space used as the unselected border.
const UNSELECTED_BORDER: &str = " ";

/// Builds the list of lines for the pinned entries panel.
fn position_badge(position: PinPosition) -> (&'static str, Color) {
    match position {
        PinPosition::Top => ("[TOP]", Color::Cyan),
        PinPosition::Bottom => ("[BOT]", Color::Magenta),
        PinPosition::Relative => ("[REL]", Color::DarkGray),
    }
}

/// Returns the display prefix and truncated content for a chat entry kind.
fn entry_prefix_and_content(kind: &ChatEntryKind) -> (&'static str, String) {
    match kind {
        ChatEntryKind::User { display, .. } => ("> ", truncate_str(display, 40)),
        ChatEntryKind::Assistant(text) => ("\u{2666} ", truncate_str(text, 40)),
        ChatEntryKind::System(text) => ("\u{2699} ", truncate_str(text, 40)),
        ChatEntryKind::Error(text) => ("\u{26a0} ", truncate_str(text, 40)),
        ChatEntryKind::Actor { source, text } => {
            let content = format!("[{}] {}", source, truncate_str(text, 30));
            ("", content)
        }
        ChatEntryKind::ToolCall { name, .. } => {
            ("\u{2692} ", format!("{}(...)", truncate_str(name, 20)))
        }
        ChatEntryKind::ToolResult {
            name,
            content,
            status,
            ..
        } => {
            // Loaded skills are pinned as `<skill name="X" ...>` XML.
            // Show a clean single-line label instead of the raw XML.
            if *name == "skill" {
                return ("", truncate_str(&loaded_skill_summary_label(content), 40));
            }
            let icon = if *status == ToolResultStatus::Success {
                "✓"
            } else {
                "✗"
            };
            (
                "",
                format!(
                    "{} {}: {}",
                    icon,
                    truncate_str(name, 15),
                    truncate_str(content, 20)
                ),
            )
        }
        // Table entries and annotations are not shown in the pinned panel summary.
        ChatEntryKind::Compaction { .. } | ChatEntryKind::Annotation { .. } => ("", String::new()),
        // Thinking entries are not shown in the pinned panel summary.
        ChatEntryKind::Thinking(text) => ("", truncate_str(text, 40)),

        ChatEntryKind::Transient(s) => ("\u{2139} ", truncate_str(s, 40)),
    }
}

/// Truncates a string to the given max grapheme length, appending an ellipsis if needed.
/// Truncate a string to at most `max_width` display columns, appending \u{2026} if truncated.
///
/// Uses `unicode_width` to measure each grapheme's display width. Wide characters
/// (emoji, CJK) that would overflow the budget are skipped entirely rather than
/// rendered partially.
pub(crate) fn truncate_to_width(s: &str, max_width: usize) -> String {
    use unicode_segmentation::UnicodeSegmentation;
    use unicode_width::UnicodeWidthStr;

    let s_width = UnicodeWidthStr::width(s);
    if s_width <= max_width {
        return s.to_owned();
    }

    // Zero budget: no room for anything, not even an ellipsis.
    if max_width == 0 {
        return String::new();
    }

    // Walk graphemes, accumulating display width.
    // Stop before a grapheme that would overflow the budget
    // (leaving room for the ellipsis).
    let ellipsis_width = 1; // \u{2026} is 1 cell
    let target_width = max_width.saturating_sub(ellipsis_width);

    let mut accumulated = 0usize;
    let mut result = String::new();
    for g in s.graphemes(true) {
        let gw = UnicodeWidthStr::width(g);
        if accumulated + gw > target_width {
            break;
        }
        result.push_str(g);
        accumulated += gw;
    }
    result.push('\u{2026}');
    result
}

/// Strip ANSI escape sequences and truncate to at most `max_width` display columns.
pub(crate) fn truncate_str(s: &str, max_width: usize) -> String {
    truncate_to_width(&strip_ansi(s), max_width)
}

/// Builds the list of lines for the pinned entries panel.
fn build_entry_list(
    pinned: &[&jinn_domain::protocol::ChatEntry],
    selected_index: usize,
    area_width: u16,
    sidebar_focused: bool,
    section_focused: bool,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    // Header
    lines.push(Line::from(vec![Span::styled(
        format!(" Pinned Context — {}", pinned.len()),
        Style::default()
            .fg(theme.primary_text)
            .add_modifier(Modifier::BOLD),
    )]));
    lines.push(Line::from(""));

    // Fixed overhead per entry line: border(1) + space(1) + badge(5) + space(1) = 8 cells.
    let fixed_overhead: u16 = 8;
    let content_budget = area_width.saturating_sub(fixed_overhead) as usize;

    for (i, entry) in pinned.iter().enumerate() {
        let is_selected = section_focused && i == selected_index;

        let indicator_color = if sidebar_focused {
            theme.focus_accent
        } else {
            theme.border_unfocused
        };
        let border = if is_selected {
            Span::styled(SELECTED_INDICATOR, Style::default().fg(indicator_color))
        } else {
            Span::raw(UNSELECTED_BORDER)
        };

        let (badge_text, badge_color) =
            position_badge(entry.pin_position.unwrap_or(PinPosition::Relative));

        let (prefix, content) = entry_prefix_and_content(&entry.kind);

        // Truncate the assembled content to the remaining cell budget.
        let full_content = format!("{prefix}{content}");
        let capped_content = truncate_to_width(&full_content, content_budget);

        let style = if is_selected {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
        };

        lines.push(Line::from(vec![
            border,
            Span::styled(format!(" {badge_text} "), Style::default().fg(badge_color)),
            Span::styled(capped_content, style),
        ]));
    }

    lines
}
