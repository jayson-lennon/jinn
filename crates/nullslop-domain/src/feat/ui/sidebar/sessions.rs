//! [`SessionsSection`] — the open sessions sidebar section.
//!
//! Implements [`SidebarSection`] for listing all sessions currently loaded
//! into memory. The active session (currently displayed) is highlighted with
//! a `▸` prefix. Navigating with j/k immediately switches the active session.

use std::time::{Duration, Instant};

use crate::common::app_state::AppState;
use crate::feat::session::chat_session::SessionPhase;
use crate::feat::ui::sidebar::section_trait::{
    EnterFrom, SectionNavResult, SidebarIntent, SidebarSection, SidebarSectionId,
};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use throbber_widgets_tui::ThrobberState;

/// Active session indicator prefix.
const ACTIVE_PREFIX: &str = "▸ ";
/// Inactive session prefix (two spaces to align with `ACTIVE_PREFIX`).
const INACTIVE_PREFIX: &str = "  ";
/// Maximum number of session entries visible at once.
const MAX_VISIBLE_SESSIONS: usize = 15;
/// Minimum time between animation frame advances.
const ANIMATION_INTERVAL: Duration = Duration::from_millis(80);

/// Sessions section cursor state — stored on `FrontendState`.
///
/// Tracks the selected index within the sorted open sessions list.
/// `None` means no cursor (section not focused).
#[derive(Debug, Clone, Default)]
pub struct SessionsSectionState {
    /// Index into the sorted open sessions list.
    pub selected_index: Option<usize>,
    /// Scroll offset: the first session entry index that is visible.
    pub scroll_offset: usize,
}

pub(crate) struct SessionEntry {
    pub(crate) id: crate::protocol::SessionId,
    pub(crate) title: String,
    pub(crate) is_active: bool,
    pub(crate) created_at: jiff::Timestamp,
    pub(crate) is_idle: bool,
    pub(crate) last_entry_is_error: bool,
}

/// Collects all open sessions sorted by `created_at` descending (newest first).
pub(crate) fn sorted_open_sessions(state: &AppState) -> Vec<SessionEntry> {
    let active_id = state.session.active_session_id();
    let mut entries: Vec<SessionEntry> = state
        .session
        .sessions()
        .iter()
        .map(|(id, session): (&_, &_)| SessionEntry {
            id: id.clone(),
            title: session.title().unwrap_or("Untitled Session").to_owned(),
            is_active: id == active_id,
            created_at: *session.created_at(),
            is_idle: matches!(session.phase(), SessionPhase::Idle),
            last_entry_is_error: session
                .history()
                .last()
                .is_some_and(|e| matches!(&e.kind, crate::protocol::ChatEntryKind::Error(..))),
        })
        .collect();
    entries.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    entries
}

/// Adjusts scroll offset to ensure the selected index is visible within the window.
///
/// If no index is selected, does nothing.
pub fn scroll_to_cursor(state: &mut AppState) {
    let Some(index) = state.frontend.sessions_section.selected_index else {
        return;
    };
    let total = sorted_open_sessions(state).len();
    let visible = MAX_VISIBLE_SESSIONS.min(total);
    if visible == 0 {
        return;
    }
    let offset = &mut state.frontend.sessions_section.scroll_offset;

    if index < *offset {
        *offset = index;
    } else if index >= *offset + visible {
        *offset = index - visible + 1;
    }
}

/// Navigate within the sessions section.
///
/// Moves the cursor within the sessions list and immediately switches
/// the active session. Returns `Exhausted` when at a boundary or when
/// the list is empty.
pub fn navigate(intent: &SidebarIntent, state: &mut AppState) -> SectionNavResult {
    let sessions = sorted_open_sessions(state);
    if sessions.is_empty() {
        return SectionNavResult::Exhausted;
    }

    let result = match intent {
        SidebarIntent::MoveDown => {
            let current = state.frontend.sessions_section.selected_index.unwrap_or(0);
            if current >= sessions.len() - 1 {
                return SectionNavResult::Exhausted;
            }
            let new_index = current + 1;
            state.frontend.sessions_section.selected_index = Some(new_index);
            SectionNavResult::Moved
        }
        SidebarIntent::MoveUp => {
            let current = state.frontend.sessions_section.selected_index.unwrap_or(0);
            if current == 0 {
                return SectionNavResult::Exhausted;
            }
            let new_index = current - 1;
            state.frontend.sessions_section.selected_index = Some(new_index);
            SectionNavResult::Moved
        }
        SidebarIntent::Action(_) => SectionNavResult::Moved,
    };

    scroll_to_cursor(state);
    result
}

/// Place the cursor on this section from a given direction.
///
/// Positions at the edge of the list: index 0 from top, last index from bottom.
/// This keeps the linear `j`/`k` scroll model consistent.
pub fn receive_cursor(state: &mut AppState, enter_from: EnterFrom) {
    let sessions = sorted_open_sessions(state);
    if sessions.is_empty() {
        return;
    }
    let index = match enter_from {
        EnterFrom::Top => 0,
        EnterFrom::Bottom => sessions.len() - 1,
    };
    state.frontend.sessions_section.selected_index = Some(index);
    scroll_to_cursor(state);
}

/// Activates the session under the cursor.
///
/// Called when the user presses Enter in the sessions section.
/// Switches `active_session` to the session at the cursor position.
pub fn handle_session_activate(state: &mut AppState) {
    use crate::common::app_state::FocusScope;
    use crate::feat::ui::sidebar::section_trait::SidebarSectionId;

    if !matches!(
        state.frontend.scope_stack.sidebar_section(),
        Some(SidebarSectionId::Sessions)
    ) {
        return;
    }
    let Some(index) = state.frontend.sessions_section.selected_index else {
        return;
    };
    let sessions = sorted_open_sessions(state);
    let Some(entry) = sessions.get(index) else {
        return;
    };
    state.session.set_active(entry.id.clone());
    // Switch to insert mode so the user can start typing immediately.
    state.frontend.scope_stack.push(FocusScope::Input);
}

/// The open sessions sidebar section.
///
/// Renders all sessions loaded into memory with the active session highlighted.
#[derive(Debug)]
pub struct SessionsSection {
    /// Animation state for the working indicator.
    throbber_state: ThrobberState,
    /// Timestamp of the last animation frame advance.
    last_animation_step: Instant,
}

impl Default for SessionsSection {
    fn default() -> Self {
        Self {
            throbber_state: ThrobberState::default(),
            last_animation_step: Instant::now(),
        }
    }
}

impl SessionsSection {
    /// Creates a new sessions section.
    pub fn new() -> Self {
        Self::default()
    }

    /// Advances the animation frame if enough time has elapsed.
    fn maybe_advance_animation(&mut self) {
        if self.last_animation_step.elapsed() >= ANIMATION_INTERVAL {
            self.throbber_state.calc_next();
            self.last_animation_step = Instant::now();
        }
    }
}

impl SidebarSection for SessionsSection {
    fn id(&self) -> SidebarSectionId {
        SidebarSectionId::Sessions
    }

    #[allow(clippy::too_many_lines)]
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, state: &AppState) {
        let sessions = sorted_open_sessions(state);
        let theme = &state.frontend.theme;
        let sidebar_focused = state.frontend.scope_stack.is_sidebar();
        let section_focused = sidebar_focused
            && matches!(
                state.frontend.scope_stack.sidebar_section(),
                Some(SidebarSectionId::Sessions)
            );

        let selected_index = state.frontend.sessions_section.selected_index;
        let scroll_offset = state.frontend.sessions_section.scroll_offset;

        let mut lines = Vec::new();

        // Header.
        lines.push(Line::from(vec![Span::styled(
            " Sessions",
            Style::default()
                .fg(theme.primary_text)
                .add_modifier(Modifier::BOLD),
        )]));

        // Blank separator.
        lines.push(Line::from(""));

        if sessions.is_empty() {
            lines.push(Line::from(vec![Span::styled(
                " No open sessions",
                Style::default().fg(theme.muted_text),
            )]));
        } else {
            let visible_count = MAX_VISIBLE_SESSIONS.min(sessions.len());
            let start = scroll_offset.min(sessions.len());
            let end = (start + visible_count).min(sessions.len());

            for (visual_i, entry) in sessions[start..end].iter().enumerate() {
                let i = start + visual_i; // absolute index for selection check
                let is_selected = section_focused && selected_index == Some(i);

                // Indicator: animated throbber when working, blank space when idle.
                let indicator_span = if entry.is_idle {
                    Span::raw(" ")
                } else {
                    let set = throbber_widgets_tui::symbols::throbber::BRAILLE_EIGHT;
                    let mut idx = self.throbber_state.index();
                    let len = set.symbols.len() as i8;
                    idx %= len;
                    if idx < 0 {
                        idx += len;
                    }
                    let ch = set.symbols[idx as usize];
                    Span::styled(ch.to_string(), Style::default().fg(Color::Cyan))
                };

                // Arrow: active session indicator.
                let arrow_span = if entry.is_active {
                    Span::styled(
                        ACTIVE_PREFIX.to_owned(),
                        Style::default().fg(theme.primary_text),
                    )
                } else {
                    Span::styled(INACTIVE_PREFIX.to_owned(), Style::default())
                };

                let title_style = if entry.last_entry_is_error {
                    if is_selected {
                        Style::default()
                            .fg(Color::Red)
                            .add_modifier(Modifier::REVERSED)
                    } else {
                        Style::default().fg(Color::Red)
                    }
                } else if is_selected {
                    Style::default().add_modifier(Modifier::REVERSED)
                } else if entry.is_active {
                    Style::default().fg(theme.primary_text)
                } else {
                    Style::default().fg(theme.muted_text)
                };

                // Truncate title to fit sidebar width (indicator(1) + prefix(2) + 1 padding).
                let max_title_len = area.width.saturating_sub(5) as usize;
                let truncated = truncate_str(&entry.title, max_title_len);

                lines.push(Line::from(vec![
                    indicator_span,
                    Span::raw(" "),
                    arrow_span,
                    Span::styled(truncated, title_style),
                ]));
            }

            // Advance animation only when enough time has elapsed.
            self.maybe_advance_animation();

            // Scroll indicators.
            let lines_above = scroll_offset;
            let lines_below = sessions
                .len()
                .saturating_sub(scroll_offset)
                .saturating_sub(visible_count);

            if lines_above > 0 || lines_below > 0 {
                let indicator_style = Style::default().fg(Color::Black).bg(theme.age_fresh);

                if lines_above > 0 {
                    let indicator_row = area.y + 2; // header + blank
                    let label = "\u{2191}"; // ↑
                    let indicator_width = 1u16;
                    let indicator_area = Rect {
                        x: area.x + area.width.saturating_sub(indicator_width),
                        y: indicator_row,
                        width: indicator_width,
                        height: 1,
                    };
                    frame.render_widget(
                        Paragraph::new(Line::from(Span::styled(label, indicator_style))),
                        indicator_area,
                    );
                }

                if lines_below > 0 {
                    let last_entry_row = area.y + 2 + visible_count as u16 - 1;
                    let label = "\u{2193}"; // ↓
                    let indicator_width = 1u16;
                    let indicator_area = Rect {
                        x: area.x + area.width.saturating_sub(indicator_width),
                        y: last_entry_row,
                        width: indicator_width,
                        height: 1,
                    };
                    frame.render_widget(
                        Paragraph::new(Line::from(Span::styled(label, indicator_style))),
                        indicator_area,
                    );
                }
            }
        }

        // Trailing gap.
        lines.push(Line::from(""));

        let widget = Paragraph::new(lines).block(Block::default().borders(Borders::NONE));
        frame.render_widget(widget, area);
    }

    fn content_height(&self, state: &AppState) -> u16 {
        let session_count = state.session.sessions().len() as u16;
        let visible = session_count.min(MAX_VISIBLE_SESSIONS as u16);
        // header(1) + blank(1) + visible sessions(N) + trailing gap(1)
        3 + visible.max(1) // max(1) for "No open sessions" message
    }
}

/// Truncates a string to fit within `max_len` graphemes, appending `…` if truncated.
fn truncate_str(s: &str, max_len: usize) -> String {
    use unicode_segmentation::UnicodeSegmentation;
    if max_len == 0 {
        return String::new();
    }
    let graphemes: Vec<&str> = s.graphemes(true).collect();
    if graphemes.len() <= max_len {
        return s.to_owned();
    }
    let mut result: String = graphemes[..max_len.saturating_sub(1)].concat();
    result.push('…');
    result
}
// ---------------------------------------------------------------------------
// Close session handler
// ---------------------------------------------------------------------------

/// Why a session close can be rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionCloseError {
    /// The sessions section is not focused.
    WrongSection,
    /// No session is selected.
    NoSelection,
    /// The selected session is streaming or sending.
    SessionBusy,
}

/// Validates that a session close can proceed.
///
/// # Errors
///
/// Returns [`SessionCloseError`] if the sessions section is not focused, no session is selected, or the session is busy.
pub fn validate_session_close(state: &AppState) -> Result<(), SessionCloseError> {
    use crate::feat::ui::sidebar::section_trait::SidebarSectionId;

    // Sessions section must be focused.
    if !matches!(
        state.frontend.scope_stack.sidebar_section(),
        Some(SidebarSectionId::Sessions)
    ) {
        return Err(SessionCloseError::WrongSection);
    }

    // A session must be selected.
    let index = state
        .frontend
        .sessions_section
        .selected_index
        .ok_or(SessionCloseError::NoSelection)?;

    // The selected session must be idle (not streaming/sending).
    let sessions = sorted_open_sessions(state);
    let entry = sessions.get(index).ok_or(SessionCloseError::NoSelection)?;
    let session = state
        .session
        .sessions()
        .get(&entry.id)
        .ok_or(SessionCloseError::NoSelection)?;
    if !matches!(session.phase(), SessionPhase::Idle) {
        return Err(SessionCloseError::SessionBusy);
    }

    Ok(())
}

/// Handles `SidebarSessionClose` — closes the selected session.
///
/// Removes the session from the in-memory HashMap (keeps it in SQLite).
/// Activates the next session in the sorted list, clamping the index.
/// If the last session is closed, creates a new empty session.
///
/// # Panics
///
/// Panics if the selected index is out of bounds (should not happen after validation).
pub fn handle_session_close(state: &mut AppState) -> crate::protocol::IntentResult {
    // Validate.
    if validate_session_close(state).is_err() {
        return crate::protocol::IntentResult::empty();
    }

    let index = state.frontend.sessions_section.selected_index.unwrap();
    let sessions = sorted_open_sessions(state);
    let closing_id = sessions[index].id.clone();
    let was_active = sessions[index].is_active;

    // Remove from HashMap (keeps in SQLite).
    state.session.sessions_mut().remove(&closing_id);

    if state.session.sessions().is_empty() {
        // Last session — create a new one with the last-used model/strategy.
        let new_session = {
            let model = state
                .frontend
                .preferences
                .last_model
                .clone()
                .unwrap_or_else(|| crate::feat::provider_infra::NO_PROVIDER_ID.to_owned());
            let strategy = state
                .frontend
                .preferences
                .last_strategy
                .as_deref()
                .map_or_else(
                    crate::protocol::PromptStrategyId::passthrough,
                    crate::protocol::PromptStrategyId::new,
                );
            let token_budget = state.frontend.preferences.context_token_budget.budget;
            let sliding_window_size = state.frontend.preferences.context_sliding_window.size;
            crate::feat::session::chat_session::ChatSessionState::new_with_profile(
                crate::feat::session::profile::SessionProfile::from_config(
                    model,
                    strategy,
                    token_budget,
                    sliding_window_size,
                ),
            )
        };
        let new_id = new_session.session_id().clone();
        state
            .session
            .sessions_mut()
            .insert(new_id.clone(), new_session);
        state.session.set_active(new_id);
        state.frontend.sessions_section.selected_index = Some(0);
    } else if was_active {
        // Closed the active session — activate next one. Clamp index to valid range.
        let remaining = sorted_open_sessions(state);
        let clamped = index.min(remaining.len() - 1);
        state.session.set_active(remaining[clamped].id.clone());
        state.frontend.sessions_section.selected_index = Some(clamped);
    } else {
        // Closed a non-active session — keep active session, clamp cursor.
        let remaining = sorted_open_sessions(state);
        let clamped = index.min(remaining.len() - 1);
        state.frontend.sessions_section.selected_index = Some(clamped);
    }

    scroll_to_cursor(state);

    crate::protocol::IntentResult::empty()
}

/// Handles `SidebarSessionClose` — closes the selected session.
///
/// Validates that the close can proceed, gets the selected session ID,
/// then emits a `CloseSession` command. The session actor handles teardown,
/// archival, removal, and emits `SessionClosed` for the sidebar actor to
/// clamp the cursor.
///
/// # Panics
///
/// Panics if `sessions_section.selected_index` is `None`.
pub fn handle_session_close_with_lifecycle(state: &mut AppState) -> crate::protocol::IntentResult {
    use crate::feat::session::protocol::close_session::CloseSession;
    use crate::protocol::Command;

    // Validate.
    if validate_session_close(state).is_err() {
        return crate::protocol::IntentResult::empty();
    }

    let index = state.frontend.sessions_section.selected_index.unwrap();
    let sessions = sorted_open_sessions(state);
    let closing_id = sessions[index].id.clone();

    // Emit CloseSession — the actor handles teardown, archive, and removal.
    crate::protocol::IntentResult::with_commands(vec![Command::CloseSession(CloseSession {
        session_id: closing_id,
    })])
}

/// Handles `SidebarSessionArchive` — archives the selected session without teardown.
///
/// Validates that the archive can proceed, gets the selected session ID,
/// then emits an `ArchiveSession` command. The session actor handles archival
/// and removal, skipping lifecycle teardown.
///
/// # Panics
///
/// Panics if `sessions_section.selected_index` is `None`.
pub fn handle_session_archive(state: &mut AppState) -> crate::protocol::IntentResult {
    use crate::feat::session::protocol::archive_session::ArchiveSession;
    use crate::protocol::Command;

    // Validate — same preconditions as session close.
    if validate_session_close(state).is_err() {
        return crate::protocol::IntentResult::empty();
    }

    let index = state.frontend.sessions_section.selected_index.unwrap();
    let sessions = sorted_open_sessions(state);
    let target_id = sessions[index].id.clone();

    // Emit ArchiveSession — the actor handles archive and removal without teardown.
    crate::protocol::IntentResult::with_commands(vec![Command::ArchiveSession(
        ArchiveSession {
            session_id: target_id,
        },
    )])
}

/// Handles `SidebarSessionTeardown` — re-runs teardown without closing the session.
///
/// Validates that the close can proceed, looks up the selected session's
/// teardown command, and emits `RunSessionTeardown` with `close_on_success: false`.
/// If the session has no teardown command, this is a no-op.
///
/// # Panics
/// Panics if `sessions_section.selected_index` is `None`.
pub fn handle_session_teardown(state: &mut AppState) -> crate::protocol::IntentResult {
    use crate::feat::session_lifecycle::command_template::CommandTemplate;

    // Validate — same preconditions as session close.
    if validate_session_close(state).is_err() {
        return crate::protocol::IntentResult::empty();
    }

    let index = state.frontend.sessions_section.selected_index.unwrap();
    let sessions = sorted_open_sessions(state);
    let target_id = sessions[index].id.clone();

    // Look up teardown command for the session.
    let (teardown_command, lifecycle_args) = {
        let session = state.session.sessions().get(&target_id);
        let Some(session) = session else {
            return crate::protocol::IntentResult::empty();
        };
        let lifecycle_name = session.lifecycle_name().map(String::from);
        let args = session.lifecycle_args().to_vec();
        let teardown = lifecycle_name.as_deref().and_then(|name| {
            state
                .frontend
                .preferences
                .session_lifecycles
                .iter()
                .find(|l| l.name == name)
                .and_then(|l| l.teardown_command.clone())
        });
        (teardown, args)
    };

    let Some(ref teardown_cmd) = teardown_command else {
        return crate::protocol::IntentResult::empty();
    };

    let template = CommandTemplate::parse(teardown_cmd);
    let rendered = if lifecycle_args.is_empty() {
        teardown_cmd.to_owned()
    } else {
        template.render(&lifecycle_args)
    };

    crate::protocol::IntentResult::with_commands(vec![
        crate::protocol::Command::RunSessionTeardown(
            crate::feat::session_lifecycle::protocol::command::RunSessionTeardown {
                session_id: target_id,
                command: rendered,
                args: lifecycle_args,
                close_on_success: false,
            },
        ),
    ])
}

/// Handles `SidebarSessionNewWithLifecycle` — opens the lifecycle picker
/// when the sessions section is focused.
///
/// No-op if the sessions section is not focused.
pub fn handle_sidebar_session_new_with_lifecycle(
    state: &mut AppState,
) -> crate::protocol::IntentResult {
    use crate::feat::ui::sidebar::section_trait::SidebarSectionId;
    use crate::protocol::PickerKind;

    if !matches!(
        state.frontend.scope_stack.sidebar_section(),
        Some(SidebarSectionId::Sessions)
    ) {
        return crate::protocol::IntentResult::empty();
    }
    crate::feat::picker::intent::handle_open_picker(state, PickerKind::SessionLifecycle)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing)]
    use super::*;
    use crate::common::app_state::{AppState, FocusScope};
    use crate::feat::session::chat_session::ChatSessionState;
    use crate::feat::session::protocol::archive_session::ArchiveSession;
    use crate::protocol::Command;

    fn setup_sessions_sidebar_with_two_sessions() -> AppState {
        let mut state = AppState::default();
        // Remove the default session so we control exact state.
        let default_id = state.session.active_session_id().clone();
        state.session.sessions_mut().remove(&default_id);

        // Add two sessions.
        let s1 = ChatSessionState::new();
        let s1_id = s1.session_id().clone();
        let s2 = ChatSessionState::new();
        let s2_id = s2.session_id().clone();
        state.session.sessions_mut().insert(s1_id, s1);
        state.session.sessions_mut().insert(s2_id.clone(), s2);
        state.session.set_active(s2_id);

        // Focus sidebar on sessions section with cursor at 0.
        state.frontend.scope_stack.push(FocusScope::SidebarSessions);
        state.frontend.sessions_section.selected_index = Some(0);

        state
    }

    #[test]
    fn archive_session_returns_command_when_valid() {
        // Given a valid state: sessions section focused, session selected, idle.
        let mut state = setup_sessions_sidebar_with_two_sessions();

        // When handling the archive intent.
        let result = handle_session_archive(&mut state);

        // Then it returns an ArchiveSession command.
        assert_eq!(result.commands.len(), 1);
        assert!(matches!(
            &result.commands[0],
            Command::ArchiveSession(ArchiveSession { .. })
        ));
    }

    #[test]
    fn archive_session_rejected_when_not_in_sessions_section() {
        // Given a state NOT in the sessions section (Normal scope).
        let mut state = AppState::default();

        // When handling the archive intent.
        let result = handle_session_archive(&mut state);

        // Then no commands are emitted.
        assert!(result.commands.is_empty());
    }

    #[test]
    fn archive_session_rejected_when_no_selection() {
        // Given sessions section focused but no selected index.
        let mut state = setup_sessions_sidebar_with_two_sessions();
        state.frontend.sessions_section.selected_index = None;

        // When handling the archive intent.
        let result = handle_session_archive(&mut state);

        // Then no commands are emitted.
        assert!(result.commands.is_empty());
    }

    #[test]
    fn archive_session_rejected_when_session_busy() {
        // Given sessions section focused, session selected, but session is streaming.
        let mut state = setup_sessions_sidebar_with_two_sessions();

        // Make the selected session streaming.
        let sessions = sorted_open_sessions(&state);
        let target_id = sessions[0].id.clone();
        state
            .session
            .sessions_mut()
            .get_mut(&target_id)
            .expect("session exists")
            .begin_streaming();

        // When handling the archive intent.
        let result = handle_session_archive(&mut state);

        // Then no commands are emitted.
        assert!(result.commands.is_empty());
    }
}
