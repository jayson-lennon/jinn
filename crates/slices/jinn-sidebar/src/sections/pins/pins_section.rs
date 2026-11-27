//! [`PinsSection`] - the pinned entries sidebar section.
//!
//! Implements [`SidebarSection`] for pinned context entries.
//! Also provides handler functions that the `IntentHandler` calls
//! for sidebar and pins intents.

use crate::sections::section_trait::{
    EnterFrom, SectionNavResult, SidebarIntent, SidebarSection, SidebarSectionId,
};
use jinn_core_types::SessionId;
use jinn_kernel::common::app_state::AppState;
use jinn_kernel::protocol::ChatEntryId;
use jinn_kernel::protocol::ChatEntryKind;
use jinn_kernel::protocol::{IntentResult, PinPosition};
use jinn_session_history_msg::{PinChatEntry, UnpinChatEntry};
use jinn_session_state::{PinDisplayRow, truncate_to_width};
use jinn_slices::DrawContext;
use jinn_slices::route::ScopeSignal;
use jinn_theme::Theme;
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
        jinn_sidebar_msg::SidebarSectionId::Pins
    }

    fn render(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        skip_rows: u16,
        ctx: &dyn DrawContext<jinn_kernel::common::app_state::AppState>,
    ) {
        let state = ctx.state();
        let sorted_ids = state.sorted_pinned_ids();
        let rows = state.active_session().pinned_display_rows();

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
        let lines = if rows.is_empty() {
            vec![Line::from(vec![Span::styled(
                " Pinned Context - 0",
                Style::default()
                    .fg(state.frontend.theme.primary_text)
                    .add_modifier(Modifier::BOLD),
            )])]
        } else {
            let section_focused = state.frontend.is_sidebar()
                && matches!(
                    state.frontend.sidebar_section(),
                    Some(jinn_sidebar_msg::SidebarSectionId::Pins)
                );
            build_entry_list(
                &rows,
                selected_index,
                area.width,
                section_focused,
                &state.frontend.theme,
                |id| {
                    state
                        .active_session()
                        .history()
                        .iter()
                        .find(|entry| entry.id == *id)
                        .map(|entry| &entry.kind)
                },
            )
        };

        // The sidebar document decides which window of these lines is visible,
        // so the section only has to skip the rows scrolled above the column.
        let widget = Paragraph::new(lines)
            .block(Block::default().borders(Borders::NONE))
            .scroll((skip_rows, 0));
        frame.render_widget(widget, area);
    }

    fn content_height(
        &mut self,
        ctx: &dyn DrawContext<jinn_kernel::common::app_state::AppState>,
    ) -> u16 {
        let state = ctx.state();
        let count = state.active_session().pinned_display_count();
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
/// section instance. Hides (0) when there are no display rows.
///
/// [`PinsSection::content_height`]: PinsSection::content_height
#[must_use]
pub fn pins_section_content_height(state: &AppState) -> u16 {
    let count = state.active_session().pinned_display_count();
    if count == 0 {
        return 0;
    }
    // Header(1) + header-gap(1) + entries(count) + trailing gap(1).
    count as u16 + 3
}

// ---------------------------------------------------------------------------
// Intent handler functions (called by IntentHandler)
// ---------------------------------------------------------------------------

/// Handles the persona edit key - requests the persona picker when the persona
/// section is focused.
///
/// No-op if the pins section is focused. The persona picker is slice-owned and
/// seeds itself from the personas cell on scope entry, so the sidebar names the
/// picker only by its scope id and returns a signal rather than pushing the
/// scope itself: the scope stack has exactly one writer.
#[must_use]
pub fn handle_sidebar_persona_edit(state: &mut AppState) -> IntentResult {
    if !matches!(
        state.frontend.sidebar_section(),
        Some(jinn_sidebar_msg::SidebarSectionId::Persona)
    ) {
        return IntentResult::empty();
    }
    IntentResult::empty()
        .with_scope_signal(ScopeSignal::Push(jinn_persona_msg::persona_picker_scope()))
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
    let rows = state.active_session().pinned_display_rows();
    let Some(row) = rows.get(index) else {
        return IntentResult::empty();
    };
    let next = cycle_position(row.position);
    let session_id = state.session.active_session_id().clone();
    let entry_id = row.entry_id.clone();
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

    let rows = state.active_session().pinned_display_rows();

    let row = rows.get(index)?;
    Some((session_id, row.entry_id.clone()))
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

/// Builds the list of lines for the pinned entries panel.
///
/// `resolve_kind` supplies the `ChatEntryKind` for a row's entry id; it is
/// only consulted for `Verbatim` rows (tool rows render from their label).
fn build_entry_list<'a>(
    rows: &[PinDisplayRow],
    selected_index: usize,
    area_width: u16,
    section_focused: bool,
    theme: &Theme,
    resolve_kind: impl Fn(&ChatEntryId) -> Option<&'a ChatEntryKind>,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    // Header
    lines.push(Line::from(vec![Span::styled(
        format!(" Pinned Context - {}", rows.len()),
        Style::default()
            .fg(theme.primary_text)
            .add_modifier(Modifier::BOLD),
    )]));
    lines.push(Line::from(""));

    // Fixed overhead per entry line: gutter(1) + gap(1) + badge(" [TOP] " = 7) = 9 cells.
    let fixed_overhead: u16 = 9;
    let content_budget = area_width.saturating_sub(fixed_overhead) as usize;

    for (i, row) in rows.iter().enumerate() {
        let is_selected = section_focused && i == selected_index;

        let badge_text = row.badge_text();
        // The badge keeps its color as the position signal: cyan for a pin
        // at the top, magenta at the bottom, dark gray relative to the view.
        let badge_color = match row.position {
            PinPosition::Top => Color::Cyan,
            PinPosition::Bottom => Color::Magenta,
            PinPosition::Relative => Color::DarkGray,
        };

        // The label — prefix and truncated content — is the session's own,
        // shared with every surface that shows a pins row. A Verbatim row
        // resolves its entry by id for its kind prefix; tool rows render
        // from the label alone and never consult the resolver.
        let (prefix, content) = row.prefix_and_content(resolve_kind(&row.entry_id));

        // Truncate the assembled content to the remaining cell budget.
        let full_content = format!("{prefix}{content}");
        let capped_content = truncate_to_width(&full_content, content_budget);

        // The badge keeps its color as the position signal on an unselected
        // row and yields to the band on a selected one, like every state
        // color in the sidebar.
        let badge = if is_selected {
            Span::styled(format!(" {badge_text} "), Style::default())
        } else {
            Span::styled(format!(" {badge_text} "), Style::default().fg(badge_color))
        };
        // The gutter column is kept: one dark cell, then a gap cell, then the
        // badge.
        let content_width = 2 + 1 + badge_text.chars().count() + 1;
        let mut spans = vec![
            crate::sections::session_row_style::gutter_span(theme),
            crate::sections::session_row_style::gutter_span(theme),
            badge,
            Span::raw(capped_content),
        ];
        if is_selected {
            // The pad carries the band to the row's last cell — `Paragraph`
            // does not extend a line's style past the last grapheme.
            spans.push(crate::sections::session_row_style::band_pad(
                content_width,
                usize::from(area_width),
                theme,
            ));
        }
        let row = Line::from(spans);
        let row = if is_selected {
            row.style(crate::sections::session_row_style::selected_row_style(
                theme,
            ))
        } else {
            row
        };
        lines.push(row);
    }

    lines
}
