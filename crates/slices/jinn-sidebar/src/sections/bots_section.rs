//! The Bots sidebar section — sessions watching another session.
//!
//! One row per loaded bot session: line 1 is the session's name (its
//! identity — a bot has no separate label), line 2 is the body of its
//! most recent `report` call. A report that predates the parent's latest
//! activity renders muted (stale); a bot that has never reported shows
//! a distinct marker instead of an empty second line.

use crate::sections::section_trait::{EnterFrom, SectionNavResult, SidebarIntent};
use jinn_bot::section_rows::bot_rows;
use jinn_core_types::SessionId;
use jinn_kernel::AppState;
use jinn_sidebar_msg::BotSectionState;
use ratatui::style::{Color, Style};
use ratatui::text::Span;

/// The marker a bot that has never reported shows on line 2.
pub(crate) const NEVER_REPORTED_MARKER: &str = jinn_bot_msg::BotReport::EMPTY_MARKER;

/// Marks a bot still being composed, matching the sessions
/// section's glyph.
const BOT_PREPPING_SYMBOL: &str = "⏸ ";
/// Marks a bot that runs on its parent's completion, matching the
/// sessions section's glyph.
const BOT_PARENT_TRIGGER_SYMBOL: &str = "⇉ ";

/// Whether the section has any rows — an empty section collapses.
pub(crate) fn has_content(state: &AppState) -> bool {
    !bot_rows(state).is_empty()
}

/// The section's rendered row count (header + separator + rows + gap).
pub(crate) fn rows(state: &AppState) -> u16 {
    if !has_content(state) {
        return 0;
    }
    // Header + blank separator + two lines per bot + a trailing blank
    // so the section does not run straight into the one below it. The
    // render emits the same lines, and `content_height` defers here, so the
    // three cannot disagree about where the next section starts.
    let count = u16::try_from(bot_rows(state).len()).unwrap_or(u16::MAX);
    count.saturating_mul(2).saturating_add(3)
}

/// The cursor row relative to the section's first rendered row.
pub(crate) fn cursor_row(state: &AppState) -> Option<u16> {
    let index = selected_row(state)?;
    let row = bot_rows(state)
        .len()
        .checked_sub(1)
        .map_or(0, |last| row_index_of(index.min(last)));
    Some(2u16.saturating_add(row))
}

/// The section-relative row of one bot's first line.
///
/// Each bot renders two lines (name + report preview), so the
/// *n*-th bot's name line is section row `2n` below the rows block.
/// [`cursor_row`] adds the header and separator offset.
fn row_index_of(index: usize) -> u16 {
    let idx = u16::try_from(index).unwrap_or(u16::MAX);
    idx.saturating_mul(2)
}

fn selected_id(state: &AppState) -> Option<SessionId> {
    state.frontend.with_sections(
        |sections: &jinn_sidebar_msg::SidebarSections| sections.bot.selected_id.clone(),
        || None,
    )
}

/// The row the cursor is drawn on, which is where it started only because the
/// rows happen to be built in list order.
fn selected_row(state: &AppState) -> Option<usize> {
    let id = selected_id(state)?;
    row_of(state, &id)
}

/// Which row of the section's list this bot is drawn on.
///
/// The section's own list, in its own order — two screen lines per bot,
/// so the unit here is the bot, not the screen row.
#[must_use]
pub fn row_of(state: &AppState, id: &SessionId) -> Option<usize> {
    bot_rows(state).iter().position(|row| &row.session_id == id)
}

/// How many bots the section is listing.
#[must_use]
pub fn row_count(state: &AppState) -> usize {
    bot_rows(state).len()
}

/// Which row of the section's list this bot is drawn on, from the map and
/// frontend state separately.
///
/// The removal path holds both mutably at once and still needs to ask the same
/// question the render pass asks, so the section's list is resolved here rather
/// than reconstructed.
#[must_use]
pub fn row_of_split(
    session: &jinn_session_state::SessionMap,
    frontend: &jinn_kernel::state::frontend_state::FrontendState,
    id: &SessionId,
) -> Option<usize> {
    jinn_bot::section_rows::bot_rows_split(session, frontend)
        .iter()
        .position(|row| &row.session_id == id)
}

/// How many bots the section lists, from the map and frontend state
/// separately.
#[must_use]
pub fn row_count_split(
    session: &jinn_session_state::SessionMap,
    frontend: &jinn_kernel::state::frontend_state::FrontendState,
) -> usize {
    jinn_bot::section_rows::bot_rows_split(session, frontend).len()
}

/// Moves the cursor to `row`, reporting exhaustion at the edges.
fn select_row(state: &mut AppState, row: usize, max_row: usize) -> SectionNavResult {
    if row > max_row {
        return SectionNavResult::Exhausted;
    }
    let Some(id) = bot_rows(state)
        .into_iter()
        .nth(row)
        .map(|target| target.session_id)
    else {
        return SectionNavResult::Exhausted;
    };
    state
        .frontend
        .update_sections(|s| s.bot.selected_id = Some(id));
    SectionNavResult::Moved
}

/// Place the cursor on this section from a given direction.
pub(crate) fn receive_cursor(state: &mut AppState, enter_from: EnterFrom) {
    let count = bot_rows(state).len();
    if count == 0 {
        return;
    }
    let index = match enter_from {
        EnterFrom::Top => 0,
        EnterFrom::Bottom => count - 1,
    };
    if let Some(id) = bot_rows(state).get(index).map(|row| row.session_id.clone()) {
        state
            .frontend
            .update_sections(|s: &mut jinn_sidebar_msg::SidebarSections| {
                s.bot = BotSectionState {
                    selected_id: Some(id),
                    ..BotSectionState::default()
                };
            });
    }
}

/// Navigate the section's cursor, reporting exhaustion at the edges.
pub(crate) fn navigate(intent: &SidebarIntent, state: &mut AppState) -> SectionNavResult {
    let count = bot_rows(state).len();
    if count == 0 {
        return SectionNavResult::Exhausted;
    }
    let max_index = count - 1;
    let current = selected_row(state).unwrap_or(0);
    match intent {
        SidebarIntent::MoveDown => select_row(state, current.saturating_add(1), max_index),
        SidebarIntent::MoveUp => {
            if current == 0 {
                SectionNavResult::Exhausted
            } else {
                select_row(state, current - 1, max_index)
            }
        }
        SidebarIntent::Action(_) => SectionNavResult::Moved,
    }
}

/// The Bots sidebar section.
///
/// Renders a header, then two lines per bot: the session name (the
/// bot's identity — there is no separate label), then the latest
/// report body, muted when stale, or a never-reported marker.
#[derive(Debug)]
pub struct BotsSection;

impl crate::sections::section_trait::SidebarSection for BotsSection {
    fn id(&self) -> jinn_sidebar_msg::SidebarSectionId {
        jinn_sidebar_msg::SidebarSectionId::Bot
    }

    fn render(
        &mut self,
        frame: &mut ratatui::Frame<'_>,
        area: ratatui::layout::Rect,
        skip_rows: u16,
        ctx: &dyn jinn_slices::DrawContext<jinn_kernel::common::app_state::AppState>,
    ) {
        use ratatui::style::Modifier;
        use ratatui::text::Line;
        use ratatui::widgets::{Block, Paragraph};

        let state = ctx.state();
        let sidebar_focused = state.frontend.is_sidebar();
        let section_focused = sidebar_focused
            && state.frontend.sidebar_section() == Some(jinn_sidebar_msg::SidebarSectionId::Bot);
        let cursor = selected_row(state);
        let theme = &state.frontend.theme;

        let mut lines = Vec::new();
        lines.push(Line::from(vec![Span::styled(
            " Bots",
            Style::default()
                .fg(theme.primary_text)
                .add_modifier(Modifier::BOLD),
        )]));
        lines.push(Line::from(""));

        for (index, row) in bot_rows(state).into_iter().enumerate() {
            let is_selected = section_focused && cursor == Some(index);
            // The name carries no style of its own when selected: a span with
            // a hard foreground would defeat the band, and a selected row is
            // the band. Unselected, the pink marks the row as a bot.
            let name_style = if is_selected {
                Style::default()
            } else {
                Style::default().fg(theme.bot_fg)
            };
            // Ahead of the name, where the sessions section puts the same
            // markers: the eye reads a marker before the title. And beside
            // the name rather than inside it — the name is what a rename
            // replaces, so a marker must stay out of its reach. Like every
            // state color they yield to the selection band when selected.
            let prepping = marker_span(
                row.is_prepping,
                BOT_PREPPING_SYMBOL,
                theme.bot_paused,
                is_selected,
            );
            let parent_trigger = marker_span(
                row.fires_on_parent_completion,
                BOT_PARENT_TRIGGER_SYMBOL,
                theme.bot_parent_trigger,
                is_selected,
            );
            // Content is shifted one column right of the header: gutter(1),
            // gap(1), then the markers and the name. Every marker is the same
            // width, so the count is the number of them times two.
            let content_width = 2
                + 2 * (usize::from(row.is_prepping) + usize::from(row.fires_on_parent_completion))
                + 1
                + row.name.chars().count();
            let mut row_spans = vec![
                crate::sections::session_row_style::gutter_span(theme),
                crate::sections::session_row_style::gutter_span(theme),
                prepping,
                parent_trigger,
                Span::styled(format!(" {}", row.name), name_style),
            ];
            if is_selected {
                // The pad carries the band to the row's last cell —
                // `Paragraph` does not extend a line's style past the last
                // grapheme.
                row_spans.push(crate::sections::session_row_style::band_pad(
                    content_width,
                    usize::from(area.width),
                    theme,
                ));
            }
            let row_line = Line::from(row_spans);
            let row_line = if is_selected {
                row_line.style(crate::sections::session_row_style::selected_row_style(
                    theme,
                ))
            } else {
                row_line
            };
            lines.push(row_line);

            let report_line = match &row.latest_report {
                Some(body) => {
                    let style = if row.is_stale {
                        Style::default().fg(theme.muted_text)
                    } else {
                        Style::default().fg(theme.primary_text)
                    };
                    Span::styled(truncate_report(body), style)
                }
                None => Span::styled(NEVER_REPORTED_MARKER, Style::default().fg(theme.dormant_fg)),
            };
            // The report preview sits under the name, aligned with it — the
            // same gutter(1) + gap(1) shift, then enough spaces to clear the
            // marker column.
            lines.push(Line::from(vec![
                crate::sections::session_row_style::gutter_span(theme),
                Span::raw("   "),
                report_line,
            ]));
        }

        // Trailing gap — the blank line that keeps the last report off the
        // next section's header. Counted by `rows`, like every sibling
        // section's gap.
        lines.push(Line::from(""));

        let widget = Paragraph::new(lines)
            .block(Block::default().borders(ratatui::widgets::Borders::NONE))
            .scroll((skip_rows, 0));
        frame.render_widget(widget, area);
    }

    fn content_height(
        &mut self,
        ctx: &dyn jinn_slices::DrawContext<jinn_kernel::common::app_state::AppState>,
    ) -> u16 {
        // Defers to `rows` so the height the layout reserves and the lines
        // the render draws stay one number, gap included.
        rows(ctx.state())
    }
}

/// One state marker on a bot row, or nothing when it does not apply.
///
/// A selected row is the selection band, so a hard foreground would defeat
/// it; every state color on this section yields to the band the same way.
fn marker_span(present: bool, symbol: &str, color: Color, is_selected: bool) -> Span<'static> {
    if !present {
        return Span::raw("");
    }
    if is_selected {
        Span::raw(symbol.to_owned())
    } else {
        Span::styled(symbol.to_owned(), Style::default().fg(color))
    }
}

/// Truncates a report body to one display line.
fn truncate_report(body: &str) -> String {
    const MAX: usize = 48;
    let first_line = body.lines().next().unwrap_or("");
    if first_line.chars().count() <= MAX {
        return first_line.to_owned();
    }
    let mut cut: String = first_line.chars().take(MAX.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used, reason = "test code")]
    use jinn_kernel::common::render_test::render_draw;

    use super::{BotsSection, has_content};
    use crate::sections::section_trait::SidebarSection;
    use jinn_kernel::AppState;

    /// State whose active session is `active`, holding `bot` as its
    /// child, plus an unrelated bot under some other parent.
    fn state_with(
        active: jinn_session_state::ChatSessionState,
        bot: jinn_session_state::ChatSessionState,
    ) -> AppState {
        let mut state = AppState::default_with_scope_focus();
        let active_id = active.session_id().clone();
        state.session.insert(active);
        state.session.insert(bot);
        state.session.set_active(active_id);
        state
    }

    /// A plain user session.
    fn user_session() -> jinn_session_state::ChatSessionState {
        jinn_session_state::ChatSessionState::new()
    }

    /// A bot of `parent`.
    fn bot_of(
        parent: &jinn_session_state::ChatSessionState,
    ) -> jinn_session_state::ChatSessionState {
        jinn_session_state::ChatSessionState::new_bot(parent, true)
    }

    #[rstest::rstest]
    fn the_section_shows_on_a_parent_that_has_an_bot() {
        // Given a parent session with one bot, and the parent active.
        let parent = user_session();
        let state = state_with(parent.clone(), bot_of(&parent));

        // When the section asks whether it has content.
        let content = has_content(&state);

        // Then it does — this is the screen the user is on.
        assert!(content);
    }

    #[rstest::rstest]
    fn the_section_shows_from_inside_an_bot() {
        // Given a bot with a sibling under the same parent, and the
        // first bot active.
        let parent = user_session();
        let active = bot_of(&parent);
        let mut state = state_with(active.clone(), active.clone());
        let sibling = bot_of(&parent);
        let sibling_id = sibling.session_id().clone();
        state.session.insert(sibling);
        state.session.set_active(active.session_id().clone());

        // When the section asks whether it has content.
        let content = has_content(&state);

        // Then it does. A bot is read in its parent's context, so its
        // siblings — the other reports on the same question — are what the
        // user is looking at.
        assert!(content);
        // And the sibling is the row that shows.
        let rows = jinn_bot::section_rows::bot_rows(&state);
        assert_eq!(rows.len(), 2, "both siblings belong to this context");
        assert!(rows.iter().any(|row| row.session_id == sibling_id));
    }

    #[rstest::rstest]
    fn the_section_hides_on_a_session_that_has_no_bots() {
        // Given a bystander session with no bot of its own, sitting
        // alongside another parent that does have one.
        let bystander = user_session();
        let elsewhere = user_session();
        let other_bot = bot_of(&elsewhere);
        let other_id = other_bot.session_id().clone();
        let mut state = state_with(bystander.clone(), other_bot);
        state.session.set_active(bystander.session_id().clone());

        // When the section asks whether it has content.
        let content = has_content(&state);

        // Then it does not. The other parent's bot is loaded and
        // visible in the store, but it belongs to a different context, and
        // listing it here would answer a question the user did not ask.
        assert!(!content);
        // And no row leaks through.
        let rows = jinn_bot::section_rows::bot_rows(&state);
        assert!(rows.is_empty(), "leaked the bot of another parent");
        assert!(
            !rows.iter().any(|row| row.session_id == other_id),
            "another parent's bot must not appear here"
        );
    }

    /// Renders the section over `state` and returns the buffer, so a test
    /// can read the cells the user actually sees.
    fn render_section(state: &AppState) -> ratatui::buffer::Buffer {
        let terminal = render_draw(state, 40, 20, |frame, _area, frame_ctx| {
            BotsSection.render(
                frame,
                ratatui::layout::Rect::new(0, 0, 40, 20),
                0,
                frame_ctx,
            );
        });
        terminal.backend().buffer().clone()
    }

    /// The rendered screen as text, one string per row.
    fn screen_text(buffer: &ratatui::buffer::Buffer) -> String {
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<String>>()
            .join("\n")
    }

    /// A parent with one composed bot of the given configuration.
    fn state_with_bot(trigger: jinn_bot_msg::BotTrigger, prep_mode: bool) -> AppState {
        let parent = user_session();
        let mut bot = bot_of(&parent);
        bot.set_title("reviewer".to_owned());
        bot.set_bot_trigger(trigger);
        bot.set_bot_is_prepping(prep_mode);
        state_with(parent, bot)
    }

    #[rstest::rstest]
    fn a_composing_bot_row_shows_the_marker_before_the_title() {
        // Given a bot still being composed, under an active parent.
        let state = state_with_bot(jinn_bot_msg::BotTrigger::Manual, true);

        // When the section is rendered.
        let buffer = render_section(&state);

        // Then the marker leads the name, matching the sessions section —
        // the eye reads the marker before the title.
        let text = screen_text(&buffer);
        let line = text
            .lines()
            .find(|line| line.contains('\u{23F8}'))
            .unwrap_or_else(|| panic!("no pause marker rendered: {text:?}"));
        let marker = line.find('\u{23F8}').expect("marker column");
        let name = line.find("reviewer").expect("bot name");
        assert!(marker < name, "the marker must precede the name: {line:?}");
    }

    #[rstest::rstest]
    fn the_prep_marker_sits_outside_the_title() {
        // Given a bot still being composed.
        let state = state_with_bot(jinn_bot_msg::BotTrigger::Manual, true);

        // When the section is rendered.
        let buffer = render_section(&state);

        // Then the name's own cells carry the name alone — no marker glued
        // to it. The name is what a rename replaces, so the marker must
        // live in a span the rename cannot reach.
        let text = screen_text(&buffer);
        let line = text
            .lines()
            .find(|line| line.contains("reviewer"))
            .unwrap_or_else(|| panic!("no bot row rendered: {text:?}"));
        let name = line.find("reviewer").expect("bot name");
        let marked = line
            .chars()
            .skip(name)
            .take("reviewer".len())
            .any(|c| c == '\u{23F8}');
        assert!(
            !marked,
            "the name's own cells must be the name alone: {line:?}"
        );
    }

    #[rstest::rstest]
    fn a_selected_bot_row_takes_the_selection_band_not_pink() {
        // Given a bot with the section's cursor on it.
        let parent = user_session();
        let mut bot = bot_of(&parent);
        bot.set_title("reviewer".to_owned());
        let state = state_with(parent, bot);
        state
            .frontend
            .scope_push(jinn_sidebar_msg::SidebarSectionId::Bot.focus_scope());
        let first = jinn_bot::section_rows::bot_rows(&state)
            .first()
            .map(|row| row.session_id.clone());
        state
            .frontend
            .update_sections(|s| s.bot.selected_id = first);
        let theme = state.frontend.theme.clone();

        // When the section is rendered into a buffer.
        let buffer = render_section_wide(&state, 60);

        // Then the bot's row carries the selection band...
        let band_y = (0..buffer.area().height)
            .find(|&y| {
                (0..buffer.area().width).any(|x| {
                    buffer
                        .cell((x, y))
                        .is_some_and(|cell| cell.bg == theme.selection_fg)
                })
            })
            .unwrap_or_else(|| panic!("no selection band rendered"));
        // ...reaching the row's last cell — the band is full-width.
        let last_banded_x = (0..buffer.area().width)
            .filter(|&x| {
                buffer
                    .cell((x, band_y))
                    .is_some_and(|cell| cell.bg == theme.selection_fg)
            })
            .max();
        assert_eq!(
            last_banded_x,
            Some(buffer.area().width.saturating_sub(1)),
            "the band must reach the row's last cell"
        );
        // And the name is band text, not the bot pink — selection
        // overrides the state color.
        let text: String = (0..buffer.area().width)
            .filter_map(|x| buffer.cell((x, band_y)).map(ratatui::buffer::Cell::symbol))
            .collect();
        let name_at = text.find("reviewer").expect("bot name visible");
        let name_cell = buffer
            .cell((u16::try_from(name_at).unwrap_or(0), band_y))
            .expect("name cell");
        assert_eq!(name_cell.fg, theme.gutter_bg);
        assert_ne!(name_cell.fg, theme.bot_fg);
        // And the gutter cell stays dark against the band.
        let gutter = buffer.cell((0, band_y)).expect("gutter cell");
        assert_eq!(gutter.bg, theme.gutter_bg);
    }

    /// Renders the section into a buffer of the given width.
    fn render_section_wide(state: &AppState, width: u16) -> ratatui::buffer::Buffer {
        let height = 20u16;
        let terminal = render_draw(state, width, height, |frame, _area, frame_ctx| {
            BotsSection.render(
                frame,
                ratatui::layout::Rect::new(0, 0, width, height),
                0,
                frame_ctx,
            );
        });
        terminal.backend().buffer().clone()
    }
}
