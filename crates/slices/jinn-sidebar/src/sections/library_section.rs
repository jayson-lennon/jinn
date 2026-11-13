//! [`LibrarySection`] — the Library sidebar section.
//!
//! A flat list of sessions the user moved here with `L`. Membership is a
//! durable per-session flag owned by the session-store slice; this section
//! renders the store's cell and overlays live status from in-memory state.
//! Members are listed whether loaded or archived — an archived member renders
//! dimmed — and activating one goes through the standard load path, which
//! unarchives as a side effect.

use crate::sections::section_trait::{
    EnterFrom, SectionNavResult, SidebarIntent, SidebarSection, SidebarSectionId,
};
use jinn_kernel::common::app_state::AppState;
use jinn_session_store_msg::{LibraryState, SessionSummary, library_slot};
use jinn_slices::DrawContext;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use throbber_widgets_tui::ThrobberState;

/// The library cell's entries, most recently updated first.
///
/// Read through the state's own slice registry, the same overlay pattern the
/// sessions list uses for live term tabs: the cell lives in the shared
/// registry, and a registry that never ran (a bare test state) reads as an
/// empty library rather than a panic.
#[must_use]
pub fn entries(state: &AppState) -> Vec<SessionSummary> {
    state
        .frontend
        .slices()
        .and_then(|slices| slices.reader::<LibraryState>(&library_slot()))
        .map_or_default(|cell| cell.read().entries.clone())
}

/// The row the cursor is drawn on, or `None` when it names no listed member.
#[must_use]
pub fn row_of(state: &AppState, id: &jinn_core_types::SessionId) -> Option<usize> {
    entries(state)
        .iter()
        .position(|entry| &entry.session_id == id)
}

/// Whether the section has any rows — an empty section collapses.
pub(crate) fn has_content(state: &AppState) -> bool {
    !entries(state).is_empty()
}

/// The section's rendered row count (header + separator + rows + gap).
pub(crate) fn rows(state: &AppState) -> u16 {
    let count = entries(state).len();
    if count == 0 {
        return 0;
    }
    let count = u16::try_from(count).unwrap_or(u16::MAX);
    count.saturating_add(3)
}

/// The cursor row relative to the section's first rendered row.
pub(crate) fn cursor_row(state: &AppState) -> Option<u16> {
    let id = state
        .frontend
        .with_sections(|s| s.library.selected_id.clone(), || None)?;
    let index = u16::try_from(row_of(state, &id)?).unwrap_or(u16::MAX);
    Some(2u16.saturating_add(index))
}

fn selected_id(state: &AppState) -> Option<jinn_core_types::SessionId> {
    state
        .frontend
        .with_sections(|s| s.library.selected_id.clone(), || None)
}

/// Stores the member drawn at `row` as the cursor.
fn select_row(state: &mut AppState, row: usize, members: &[SessionSummary]) {
    if let Some(entry) = members.get(row) {
        state
            .frontend
            .update_sections(|s| s.library.selected_id = Some(entry.session_id.clone()));
    }
}

/// Navigate within the library section.
///
/// Moves the cursor within the library list. Exhausts at the boundaries so
/// the sidebar can move focus to the neighbor section; the section does not
/// touch its cursor on exhaustion.
pub fn navigate(intent: &SidebarIntent, state: &mut AppState) -> SectionNavResult {
    let members = entries(state);
    if members.is_empty() {
        return SectionNavResult::Exhausted;
    }
    let current_row = selected_id(state)
        .as_ref()
        .and_then(|id| members.iter().position(|entry| &entry.session_id == id))
        .unwrap_or(0);
    match intent {
        SidebarIntent::MoveDown => {
            let next = current_row.saturating_add(1);
            if next >= members.len() {
                return SectionNavResult::Exhausted;
            }
            select_row(state, next, &members);
            SectionNavResult::Moved
        }
        SidebarIntent::MoveUp => {
            if current_row == 0 {
                return SectionNavResult::Exhausted;
            }
            select_row(state, current_row - 1, &members);
            SectionNavResult::Moved
        }
        SidebarIntent::Action(_) => SectionNavResult::Moved,
    }
}

/// Place the cursor on this section from a given direction.
///
/// Positions at the edge of the list: index 0 from top, last index from
/// bottom. An empty library leaves the cursor unset — the section has no
/// rows for it to name.
///
/// Entry also asks the store actor to refresh the Library cell, so the rows
/// this section is about to draw reflect the store as of now. The request is
/// published unconditionally — an empty *cell* is not evidence of an empty
/// *store* — and the query runs on the store actor's own task, never the UI
/// thread; the section simply draws whatever the cell holds on the next tick.
pub fn receive_cursor(
    state: &mut AppState,
    enter_from: EnterFrom,
) -> jinn_kernel::protocol::IntentResult {
    use jinn_session_store_msg::LoadLibraryEntries;

    let members = entries(state);
    if !members.is_empty() {
        let row = match enter_from {
            EnterFrom::Top => 0,
            EnterFrom::Bottom => members.len() - 1,
        };
        select_row(state, row, &members);
    }
    jinn_kernel::protocol::IntentResult::new_message(LoadLibraryEntries)
}

/// `<enter>` — activate the highlighted member.
///
/// Extends the shared activation: [`crate::sections::sessions::
/// handle_session_activate`] reads whichever cursor belongs to the focused
/// section, and the Library arm was added to that resolver, so a member —
/// including an archived one — activates through the standard load path.
pub fn handle_library_activate(state: &mut AppState) -> jinn_kernel::protocol::IntentResult {
    crate::sections::sessions::handle_session_activate(state)
}

/// `a` — archive the highlighted member.
///
/// Membership survives the archive (nothing on the archive path writes the
/// flag), so the member stays listed, now dimmed. An already-archived member
/// is not in memory, and re-archiving it would be a no-op anyway, so the
/// handler refuses when the cursor names a session that is not live.
pub fn handle_library_archive(state: &mut AppState) -> jinn_kernel::protocol::IntentResult {
    use jinn_session_store_msg::ArchiveSession;
    use jinn_slices::PhaseView;

    if state.frontend.sidebar_section() != Some(jinn_sidebar_msg::SidebarSectionId::Library) {
        return jinn_kernel::protocol::IntentResult::empty();
    }
    let Some(target_id) = selected_id(state) else {
        return jinn_kernel::protocol::IntentResult::empty();
    };
    let Some(session) = state.session.get(&target_id) else {
        // Not in memory: already archived. Nothing to do.
        return jinn_kernel::protocol::IntentResult::empty();
    };
    if !matches!(session.phase(), PhaseView::Idle) {
        return jinn_kernel::protocol::IntentResult::empty();
    }
    crate::sections::sessions::state::mark_in_flight(state, std::slice::from_ref(&target_id));
    jinn_kernel::protocol::IntentResult::new_message(ArchiveSession {
        session_id: target_id,
    })
}

/// `L` — remove the highlighted member from the library.
///
/// Works by session id alone, so removing an archived (not-in-memory) member
/// needs no load first. A removed member reappears in the Sessions list when
/// it is loaded.
///
/// The cursor lands on the nearest surviving member in the same write: the
/// removal takes its row with it, and a cursor left naming it would resolve
/// to nothing — no selection band, and a `G`/`R`/`P` press acting on nothing.
/// Same position in the shorter list, clamped to the last row; the library
/// is emptying, the cursor clears. Written here rather than event-side: the
/// store actor publishes the membership event *before* its detached cell
/// refresh lands, so an event-side repair would read a stale cell.
pub fn handle_library_remove(state: &mut AppState) -> jinn_kernel::protocol::IntentResult {
    use jinn_session_store_msg::SetSessionLibrary;

    if state.frontend.sidebar_section() != Some(jinn_sidebar_msg::SidebarSectionId::Library) {
        return jinn_kernel::protocol::IntentResult::empty();
    }
    let Some(target_id) = selected_id(state) else {
        return jinn_kernel::protocol::IntentResult::empty();
    };
    // Predictive cursor landing: the cell still lists the leaving member
    // until the actor's refresh arrives, so the target is computed over the
    // list as it will be once the row is gone — same position in the shorter
    // list, mapped back through the removal for the read.
    let members = entries(state);
    let leaving_row = members
        .iter()
        .position(|entry| entry.session_id == target_id);
    let target = {
        let remaining = members.len().saturating_sub(1);
        match (leaving_row, remaining) {
            (Some(_), 0) => None,
            (Some(row), _) => {
                // Where the cursor sits in the list *after* the row is gone.
                let restored = row.min(remaining - 1);
                // The member that will occupy it: itself, when it sat before
                // the removal; its next neighbor, when the removal was ahead.
                let idx = if restored >= row {
                    restored + 1
                } else {
                    restored
                };
                members.get(idx).map(|entry| entry.session_id.clone())
            }
            // The cursor names nothing listed; leave it alone.
            (None, _) => selected_id(state),
        }
    };
    state
        .frontend
        .update_sections(|s| s.library.selected_id = target);
    jinn_kernel::protocol::IntentResult::new_message(SetSessionLibrary {
        session_id: target_id,
        in_library: false,
    })
}

/// The Library sidebar section.
///
/// Renders a header followed by one row per member. Members loaded into
/// memory lead with the same working indicator the sessions rows carry — a
/// braille spinner while streaming, a block while a disposal is in flight —
/// and a member whose history ends in an error wears the same red block.
/// Archived members render dimmed with a blank indicator cell; the active
/// session carries the `▸` marker the sessions list uses.
#[derive(Debug)]
pub struct LibrarySection {
    /// Animation state for the working indicator.
    throbber_state: ThrobberState,
    /// Timestamp of the last animation frame advance.
    last_animation_step: std::time::Instant,
}

impl Default for LibrarySection {
    fn default() -> Self {
        Self {
            throbber_state: ThrobberState::default(),
            last_animation_step: std::time::Instant::now(),
        }
    }
}

impl LibrarySection {
    /// Advances the animation frame if enough time has elapsed.
    ///
    /// The same cadence the sessions section ticks its own throbber on: two
    /// simultaneously rendered sections each advance their own state, which
    /// is the existing per-section pattern.
    fn maybe_advance_animation(&mut self) {
        if self.last_animation_step.elapsed() >= jinn_slices::SPINNER_INTERVAL {
            self.throbber_state.calc_next();
            self.last_animation_step = std::time::Instant::now();
        }
    }
}

/// The live facts a member row renders from, read off the in-memory session.
///
/// An archived member is not in memory, so the in-memory axes fall back to
/// their dormant values: idle, never in flight, no error tail. `is_bot` is
/// not live — it comes off the member's summary (the persisted `is_bot`
/// column), so it reads the same for archived and live members.
struct MemberStatus {
    /// Whether the member's phase is idle (absent ⇒ idle).
    is_idle: bool,
    /// Whether a disposal has been dispatched for the member.
    is_in_flight: bool,
    /// Whether the member's history ends in an error entry.
    last_entry_is_error: bool,
    /// Whether the member is a bot.
    is_bot: bool,
}

impl MemberStatus {
    /// Reads the live facts for one member from app state.
    fn of(state: &AppState, id: &jinn_core_types::SessionId, summary_is_bot: bool) -> Self {
        use jinn_slices::PhaseView;
        match state.session.get(id) {
            Some(session) => Self {
                is_idle: matches!(session.phase(), PhaseView::Idle),
                is_in_flight: crate::sections::sessions::state::is_in_flight(&state.frontend, id),
                last_entry_is_error: session.history().last().is_some_and(|entry| {
                    matches!(&entry.kind, jinn_core_types::ChatEntryKind::Error(..))
                }),
                is_bot: summary_is_bot,
            },
            // Not in memory — archived. Dormant on every live axis.
            None => Self {
                is_idle: true,
                is_in_flight: false,
                last_entry_is_error: false,
                is_bot: summary_is_bot,
            },
        }
    }
}

/// The member title's color when nothing is selected.
///
/// Priority mirrors the sessions rows: an in-flight disposal tints the title,
/// an error tail paints the red block (an inversion of the panel — the row's
/// own foreground is the sidebar background), otherwise the state color
/// applies: archived dims, live stays primary. Selection is handled by the
/// line, which restyles every span into the band.
fn member_title_style(
    theme: &jinn_theme::Theme,
    status: &MemberStatus,
    is_archived: bool,
) -> ratatui::style::Style {
    use ratatui::style::Color;
    if status.is_in_flight {
        return Style::default()
            .fg(theme.in_flight_fg)
            .bg(theme.in_flight_bg);
    }
    if status.last_entry_is_error {
        return Style::default().fg(theme.gutter_bg).bg(Color::Red);
    }
    // The bots color marks a bot row exactly where the sessions tree marks
    // one — dimmed when archived (`subagent_fg`'s role for a dormant row),
    // full-strength when live — so the same kind reads as the same kind in
    // both lists.
    if is_archived {
        let color = if status.is_bot {
            theme.subagent_fg
        } else {
            theme.muted_text
        };
        return Style::default().fg(color);
    }
    if status.is_bot {
        return Style::default().fg(theme.bot_fg);
    }
    Style::default().fg(theme.primary_text)
}

impl SidebarSection for LibrarySection {
    fn id(&self) -> SidebarSectionId {
        jinn_sidebar_msg::SidebarSectionId::Library
    }

    fn render(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        skip_rows: u16,
        ctx: &dyn DrawContext<jinn_kernel::common::app_state::AppState>,
    ) {
        // Tick the spinner before reading it, so a busy member's glyph
        // advances with the frame.
        self.maybe_advance_animation();
        let state = ctx.state();
        let theme = &state.frontend.theme;
        let sidebar_focused = state.frontend.is_sidebar();
        let section_focused = sidebar_focused
            && matches!(
                state.frontend.sidebar_section(),
                Some(jinn_sidebar_msg::SidebarSectionId::Library)
            );
        let cursor = selected_id(state);
        let active_id = state.session.active_session_id().clone();

        let mut lines = Vec::new();
        // Header.
        lines.push(Line::from(vec![Span::styled(
            " Library",
            Style::default()
                .fg(theme.primary_text)
                .add_modifier(Modifier::BOLD),
        )]));
        // Blank separator.
        lines.push(Line::from(""));

        let members = entries(state);
        if members.is_empty() {
            lines.push(Line::from(vec![Span::styled(
                " Nothing saved yet",
                Style::default().fg(theme.muted_text),
            )]));
        }
        for member in &members {
            let is_selected = section_focused && cursor.as_ref() == Some(&member.session_id);
            // The in-memory session's title wins when one exists: the rename
            // confirm writes the map synchronously, while the cell's summary
            // only catches up after the persist → actor → refresh round trip
            // (up to seconds). The cell stays authoritative for archived
            // members — not in memory, so not renamable — and the snapshot
            // converges when the refresh lands, so the overlay is invisible
            // in steady state.
            let title = state
                .session
                .get(&member.session_id)
                .and_then(|session| session.title())
                .unwrap_or(member.title.as_str());
            let archived = member.session_state == jinn_session_store_msg::SessionState::Archived;
            let status = MemberStatus::of(state, &member.session_id, member.is_bot);
            let prefix = if member.session_id == active_id {
                "▸ "
            } else {
                "  "
            };
            // The same leading indicator cell the sessions rows carry: blank
            // when idle and nothing is in flight, a braille spinner while the
            // member streams, a block while its disposal runs. An archived
            // member is dormant on both axes, so its cell is blank.
            let indicator = super::sessions::render::entry_line::indicator_span(
                status.is_idle,
                status.is_in_flight,
                &self.throbber_state,
                theme,
            );
            // The prefix yields to the band when selected, exactly like the
            // title: a span with a hard foreground would defeat the band.
            let prefix_style = if is_selected {
                Style::default()
            } else {
                member_title_style(theme, &status, archived)
            };
            let title_style = if is_selected {
                Style::default()
            } else {
                member_title_style(theme, &status, archived)
            };
            // Content is shifted one column right of the header:
            // gutter(1), gap(1), then indicator, prefix and title — the
            // indicator is one grapheme, same as the sessions rows count it.
            let content_width = 2 + 1 + prefix.chars().count() + title.chars().count();
            let mut spans = vec![
                crate::sections::session_row_style::gutter_span(theme),
                crate::sections::session_row_style::gutter_span(theme),
                indicator,
                Span::styled(prefix, prefix_style),
                Span::styled(title.to_owned(), title_style),
            ];
            if is_selected {
                // The pad carries the band to the row's last cell —
                // `Paragraph` does not extend a line's style past the last
                // grapheme.
                spans.push(crate::sections::session_row_style::band_pad(
                    content_width,
                    usize::from(area.width),
                    theme,
                ));
            }
            if status.is_in_flight {
                // The in-flight wash runs the full width of the row, the same
                // as the sessions rows, so the row reads as one tinted block.
                let wash = Style::default()
                    .fg(theme.in_flight_fg)
                    .bg(theme.in_flight_bg);
                spans = spans
                    .into_iter()
                    .map(|span| span.style(wash))
                    .collect::<Vec<_>>();
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

        let widget = Paragraph::new(lines)
            .block(Block::default().borders(Borders::NONE))
            .scroll((skip_rows, 0));
        frame.render_widget(widget, area);
    }

    fn content_height(
        &mut self,
        ctx: &dyn DrawContext<jinn_kernel::common::app_state::AppState>,
    ) -> u16 {
        rows(ctx.state())
    }
}
