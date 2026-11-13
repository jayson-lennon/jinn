//! The dashboard's VIEW artifact — draws the dashboard slice payload.
//!
//! Third of the contribution triple (STATE = [`DashboardState`] cell,
//! LOGIC = [`DashboardCanvasActor`](crate::canvas_actor::DashboardCanvasActor),
//! VIEW = [`DashboardView`]): a pure renderer over `&DashboardState`
//! plus the current theme, which stays in `AppState` because themes are
//! runtime-switchable application data, not slice payload.

use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Clear, HighlightSpacing, Paragraph, Row, Table, TableState};
use unicode_width::UnicodeWidthStr;

use crate::ActorLifecycle;
use crate::DashboardEntry;
use crate::DashboardState;
use jinn_slices::NoteTone;
use jinn_slices::SlotKey;
use jinn_slices::view::SliceView;
use jinn_slices::view::ViewCx;
use jinn_theme::Theme;

/// Cells reserved for the State column: the longest state word plus
/// padding. Fitted to the words rather than guessed, so adding a state
/// that fits still lines up and one that does not is caught by a test.
const STATE_COL: u16 = 10;

/// Cells reserved for the Name column.
///
/// A partition-set entity's path carries a UUID key
/// (`jinn.discovery/<uuid>` is 51 cells), so no width shows every name.
/// 40 fits the common `jinn.*` paths and truncates the pathological ones
/// — which is what the selected-row overlay is for.
const NAME_COL: u16 = 40;

/// Renders the dashboard slice: one row per actor, selection highlight,
/// scroll window. Owns no domain state — every drawn value comes from
/// the slice and the theme.
///
/// The theme comes in through [`ViewCx`] — it is application data
/// (runtime-switchable), not slice payload.
#[derive(Debug)]
pub struct DashboardView {
    slot: SlotKey,
}

impl DashboardView {
    /// A view over the dashboard's canonical slice slot.
    #[must_use]
    pub fn new() -> Self {
        Self {
            slot: super::dashboard_slot(),
        }
    }
}

impl Default for DashboardView {
    fn default() -> Self {
        Self::new()
    }
}

impl SliceView for DashboardView {
    type Slice = DashboardState;

    fn slot(&self) -> SlotKey {
        self.slot.clone()
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, cx: &ViewCx<'_>, slice: &Self::Slice) {
        let theme = cx.theme;
        let actors = slice.actors();
        if actors.is_empty() {
            render_empty(frame, area, theme);
            return;
        }

        // The scroll window is DERIVED, never stored: it is a pure
        // function of the cursor and this frame's viewport, so the slice
        // actor owns no scroll state and no write handle reaches the
        // render path. The cursor sits at the viewport's vertical centre
        // rather than being nudged into view, which is what makes a long
        // list scroll steadily instead of lurching a row at a time.
        let content_height = area.height.saturating_sub(1); // header row
        let offset = slice.offset_for_viewport(usize::from(content_height));
        let selected = slice.selected_index();

        let rows = build_rows(&actors, theme);
        // Notes absorbs whatever the two fixed columns leave. It is the
        // only elastic column: names are bounded and state words are
        // known, so a remainder spent on either would be wasted.
        let widths = [
            Constraint::Length(STATE_COL),
            Constraint::Length(NAME_COL),
            Constraint::Min(10),
        ];

        let header = Row::new(vec![
            Cell::from("State"),
            Cell::from("Name"),
            Cell::from("Notes"),
        ])
        .style(Style::default().add_modifier(Modifier::BOLD));

        let table = Table::new(rows, widths)
            .header(header)
            .column_spacing(2)
            .row_highlight_style(Style::default().fg(theme.focus_accent))
            .highlight_symbol("▸ ")
            .highlight_spacing(HighlightSpacing::Always);

        let mut table_state = TableState::default();
        table_state.select(Some(selected));
        *table_state.offset_mut() = offset;

        frame.render_stateful_widget(table, area, &mut table_state);

        // The overlay is drawn AFTER the table so it paints over the
        // truncated name of the selected row. Drawn second means it also
        // covers the Notes cell for that row, which is the cost of
        // breaking the 40-column maximum: a long name is worth more than
        // the status text it hides, and the full value is readable.
        if let Some(entry) = selected_truncated(&actors, selected, offset)
            && let Some(y) = row_y(area, selected, offset)
        {
            overlay_selected_name(frame, area, entry, theme, y);
        }
    }
}

/// The selected row, if it is visible AND its name does not fit the
/// column — the only case where the overlay earns its keep.
fn selected_truncated<'a>(
    actors: &[&'a DashboardEntry],
    selected: usize,
    offset: usize,
) -> Option<&'a DashboardEntry> {
    let entry = actors.get(selected)?;
    if selected < offset {
        // Scrolled out of the window: overlaying at a row the table
        // never drew would paint over the wrong actor.
        return None;
    }
    is_truncated(&entry.name, NAME_COL).then_some(entry)
}

/// Whether `name` is wider than `max` terminal cells.
fn is_truncated(name: &str, max: u16) -> bool {
    name.width() > usize::from(max)
}

/// Paints the selected row's full name over the row, breaking the
/// Name column's width so the whole value is readable.
///
/// `y` is the row's OFFSET WITHIN `area`, not an absolute buffer row.
/// The caller converts it with [`absolute_y`] when placing the rect.
fn overlay_selected_name(
    frame: &mut Frame<'_>,
    area: Rect,
    entry: &DashboardEntry,
    theme: &Theme,
    y: u16,
) {
    let width = area.width.saturating_sub(NAME_X);
    // `y` is area-relative; the buffer wants an absolute row.
    let Some(y) = absolute_y(area, y) else { return };
    if width == 0 {
        return;
    }
    let overlay_area = Rect {
        x: NAME_X + area.x,
        y,
        width,
        height: 1,
    };
    // `Clear` first: the overlay is wider than its column, so without
    // erasing, the characters underneath (the rest of the name, then the
    // Notes cell) show through the gaps.
    frame.render_widget(Clear, overlay_area);

    // `Clear` also resets styling, which would truncate the cursor row's
    // highlight at the end of the name. Filling the row with
    // highlight-styled padding keeps the selected row reading as one
    // continuous bar rather than a highlighted stub with a dead tail.
    let highlight = Style::default().fg(theme.focus_accent);
    let pad = usize::from(width).saturating_sub(entry.name.width());
    let mut spans = vec![Span::styled(entry.name.as_str(), highlight)];
    if pad > 0 {
        spans.push(Span::styled(" ".repeat(pad), highlight));
    }
    let para = Paragraph::new(Line::from(spans));
    frame.render_widget(para, overlay_area);
}

/// The x offset at which the Name column starts: the highlight symbol
/// (2 cells), the State column, and the column spacing (2).
const HIGHLIGHT: u16 = 2;
const COLUMN_SPACING: u16 = 2;
const NAME_X: u16 = HIGHLIGHT + STATE_COL + COLUMN_SPACING;

/// The row the table draws data row `index` on, RELATIVE TO `area`.
/// `None` when the row is scrolled out of the viewport — there is
/// nothing to overlay in that case.
fn row_y(area: Rect, index: usize, offset: usize) -> Option<u16> {
    let relative = index.checked_sub(offset)?;
    let y = 1 + u16::try_from(relative).ok()?;
    (y < area.height).then_some(y)
}

/// Converts an area-relative row into an absolute buffer row, rejecting
/// anything past the area's bottom edge.
///
/// The view's area does NOT start at the buffer origin. The app hands
/// the dashboard the content region below the tab bar, so `area.y` is 1.
/// Treating a relative row as absolute therefore puts the overlay one
/// row too high, on the header directly above the cursor row it is
/// meant to extend.
fn absolute_y(area: Rect, relative: u16) -> Option<u16> {
    let y = area.y.checked_add(relative)?;
    (y < area.y.saturating_add(area.height)).then_some(y)
}

/// Renders the empty-state placeholder.
fn render_empty(frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
    let para = Paragraph::new(Line::from(Span::styled(
        " No services registered.",
        Style::default().fg(theme.muted_text),
    )));
    frame.render_widget(para, area);
}

/// Builds the table rows from dashboard entries, applying per-lifecycle
/// colors.
///
/// Three columns, in this order: State, Name, Notes. The Description
/// column is gone — of roughly forty-five actors on the fabric, exactly
/// one (the discord gateway) publishes a description, so the column was
/// 44 empty cells wide. Where a feature has prose to contribute it
/// belongs in Notes, which is the elastic column and the only one that
/// can hold a sentence.
fn build_rows<'a>(actors: &[&'a DashboardEntry], theme: &Theme) -> Vec<Row<'a>> {
    actors
        .iter()
        .map(|entry| {
            let (state_str, state_color) = lifecycle_display(entry.lifecycle, theme);
            let state_cell = Cell::from(state_str).style(Style::default().fg(state_color));

            let name_cell =
                Cell::from(entry.name.as_str()).style(Style::default().fg(theme.primary_text));

            // The Notes column carries ONE thing: what the owning feature
            // last said about itself. A row with no feature behind it has
            // no note, and the empty cell is the honest rendering — a
            // census covers every actor on the fabric, and most of them
            // have nothing to report beyond their existence and state.
            let notes_cell = Cell::from(entry.status_message.as_deref().unwrap_or(""))
                .style(Style::default().fg(note_tone_color(entry.note_tone, theme)));

            Row::new(vec![state_cell, name_cell, notes_cell])
        })
        .collect()
}

/// Returns the display string and color for a lifecycle variant.
///
/// These four words are the runtime's verdicts, verbatim. There is no
/// catch-all "Dead": an actor that finished cleanly or was torn down
/// deliberately has no row at all, so every word on screen describes a
/// state a reader should act on.
fn lifecycle_display(lifecycle: ActorLifecycle, theme: &Theme) -> (&'static str, Color) {
    match lifecycle {
        ActorLifecycle::Running => ("Running", theme.success),
        // Dormant, not the error color: a passivated actor is evicted for
        // idleness and returns on the next send. Painting it like a
        // failure is how a normal idle cycle reads as an incident. Its
        // own token rather than the generic muted one, because a
        // deliberate dormancy is a specific state and the theme should be
        // able to say so without borrowing a meaning meant for prose.
        ActorLifecycle::Idle => ("Idle", theme.dormant_fg),
        ActorLifecycle::Escalated => ("Escalated", theme.error_text),
        ActorLifecycle::Crashed => ("Crashed", theme.error_text),
    }
}

/// Maps a feature's note tone onto a theme token.
///
/// The view owns this mapping deliberately: a feature crossing the bus
/// expresses intent (how loudly it wants to be read), never a colour, so
/// no publisher can hardcode a value that fights the user's theme.
fn note_tone_color(tone: NoteTone, theme: &Theme) -> Color {
    match tone {
        NoteTone::Muted => theme.muted_text,
        NoteTone::Warning => theme.warning,
        NoteTone::Error => theme.error_text,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing, reason = "test code")]
    use super::*;
    use crate::DashboardState;
    use crate::DashboardView;
    use jinn_slices::NoteTone;
    use jinn_slices::view::SliceView;
    use jinn_slices::view::ViewCx;
    use jinn_testutil::setup_term;
    use jinn_theme::default_theme;

    /// The x offset the State column starts at: the highlight symbol
    /// (2 cells) plus the column spacing after it is accounted for by the
    /// table itself, so the word begins at HIGHLIGHT.
    const STATE_X: u16 = 2;

    /// Collects the entire terminal buffer into a single string for substring
    /// assertions.
    fn buffer_string(terminal: &ratatui::Terminal<ratatui::backend::TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect()
    }

    /// Renders `slice` at the given geometry and returns the terminal.
    fn render_at(
        slice: &DashboardState,
        width: u16,
        height: u16,
    ) -> ratatui::Terminal<ratatui::backend::TestBackend> {
        let theme = default_theme();
        let cx = ViewCx { theme: &theme };
        let (mut terminal, _area) = setup_term(width, height);
        terminal
            .draw(|frame| {
                let mut view = DashboardView::new();
                let area = ratatui::layout::Rect::new(0, 0, width, height);
                view.render(frame, area, &cx, slice);
            })
            .expect("render");
        terminal
    }

    /// The text of one buffer row.
    fn row_text(terminal: &ratatui::Terminal<ratatui::backend::TestBackend>, y: u16) -> String {
        let buf = terminal.backend().buffer();
        (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect()
    }

    /// The buffer row the `▸` cursor marker landed on.
    fn cursor_row(terminal: &ratatui::Terminal<ratatui::backend::TestBackend>) -> Option<u16> {
        let buf = terminal.backend().buffer();
        (0..buf.area.height).find(|y| row_text(terminal, *y).contains('▸'))
    }

    /// The State word on a given buffer row, trimmed.
    fn state_word_on(
        terminal: &ratatui::Terminal<ratatui::backend::TestBackend>,
        y: u16,
    ) -> String {
        let buf = terminal.backend().buffer();
        (STATE_X..STATE_X + STATE_COL)
            .map(|x| buf[(x, y)].symbol())
            .collect::<String>()
            .trim_end()
            .trim()
            .to_owned()
    }

    /// The foreground colour of the first cell of a buffer row's State
    /// column.
    fn state_colour_on(
        terminal: &ratatui::Terminal<ratatui::backend::TestBackend>,
        y: u16,
    ) -> ratatui::style::Color {
        terminal.backend().buffer()[(STATE_X, y)].fg
    }

    /// The foreground colour of a note, located by the note's own text.
    ///
    /// Only DATA rows are searched: the header row also contains the word
    /// "Notes", and matching that would read a header cell instead of the
    /// note being asked about.
    fn note_colour(
        terminal: &ratatui::Terminal<ratatui::backend::TestBackend>,
        note: &str,
    ) -> ratatui::style::Color {
        let buf = terminal.backend().buffer();
        #[expect(
            clippy::panic,
            reason = "test helper: a note no row carries is a fixture bug, panic is the failure channel"
        )]
        let (y, text) = (1..buf.area.height)
            .map(|y| (y, row_text(terminal, y)))
            .find(|(_, text)| text.contains(note))
            .unwrap_or_else(|| panic!("no data row carries the note {note}"));
        // The note is ASCII in every test, so its byte offset in the row
        // is its column offset.
        let x = u16::try_from(text.find(note).expect("note present")).expect("x fits u16");
        buf[(x, y)].fg
    }

    #[rstest::rstest]
    #[test]
    fn dashboard_view_renders_rows_with_selection() {
        // Given a dashboard slice with two actors, the second selected.
        let mut slice = DashboardState::new();
        slice.mark_running("alpha", None);
        slice.mark_running("beta", Some("second".to_owned()));
        slice.select_next();
        let theme = default_theme();
        let cx = ViewCx { theme: &theme };

        // When rendering through the view.
        let (mut terminal, _area) = setup_term(80, 24);
        terminal
            .draw(|frame| {
                let mut view = DashboardView::new();
                let area = ratatui::layout::Rect::new(0, 0, 80, 24);
                view.render(frame, area, &cx, &slice);
            })
            .expect("render");

        // Then both rows are drawn and the marker sits on `beta`.
        let buf = buffer_string(&terminal);
        assert!(buf.contains("alpha"), "first row renders");
        assert!(buf.contains("beta"), "second row renders");
        assert!(buf.contains('▸'), "selection marker renders");
        assert!(buf.contains("Running"), "lifecycle column renders");
    }

    /// A passivated row must read as dormant, not as a failure. The
    /// rendered row is what a user actually sees, so the State column is
    /// pinned here rather than only in the state fold's tests.
    #[rstest::rstest]
    #[test]
    fn dashboard_view_renders_a_passivated_actor_as_idle() {
        // Given a dashboard slice whose only actor was passivated.
        let mut slice = DashboardState::new();
        slice.mark_idle("jinn.discovery/abc");

        // When rendering through the view.
        let terminal = render_at(&slice, 80, 24);

        // Then the row reads Idle.
        let buf = buffer_string(&terminal);
        assert!(buf.contains("Idle"), "idle row renders: {buf}");
    }

    /// No row may ever read the old catch-all word: a failed actor has
    /// its own named state, and a cleanly-stopped one has no row at all.
    #[rstest::rstest]
    #[test]
    fn no_row_ever_reads_dead() {
        // Given a dashboard holding one crashed actor.
        let mut slice = DashboardState::new();
        slice.mark_failed("broken", crate::ActorLifecycle::Crashed);

        // When rendering through the view.
        let terminal = render_at(&slice, 80, 24);

        // Then the word "Dead" appears nowhere on screen.
        let buf = buffer_string(&terminal);
        assert!(!buf.contains("Dead"), "the word Dead is gone: {buf}");
        // And the state cell names the actual failure.
        assert_eq!(state_word_on(&terminal, 1), "Crashed");
    }

    /// A crash is drawn in the error colour, so a real failure is
    /// visible at a glance rather than sitting among forty healthy rows.
    #[rstest::rstest]
    #[test]
    fn a_crashed_row_is_drawn_in_the_error_colour() {
        // Given a dashboard where a crashed actor is NOT the selected
        // row. The selected row's foreground is the highlight colour,
        // which overrides the cell's own, so reading the cursor row
        // would measure the highlight instead of the state.
        let mut slice = DashboardState::new();
        slice.mark_failed("broken", crate::ActorLifecycle::Crashed);
        slice.mark_running("healthy", None);
        slice.select_last();
        let theme = default_theme();

        // When rendering through the view.
        let terminal = render_at(&slice, 80, 24);

        // Then the unselected crash row's State cell carries the error
        // colour, and it is the top row because it sorted there.
        assert_eq!(state_colour_on(&terminal, 1), theme.error_text);
    }

    /// Dormancy gets its own token precisely so it cannot be mistaken
    /// for a failure.
    #[rstest::rstest]
    #[test]
    fn an_idle_row_is_drawn_in_the_dormant_colour_not_the_error_colour() {
        // Given a dashboard where a passivated actor is not selected.
        let mut slice = DashboardState::new();
        slice.mark_idle("dormant");
        slice.mark_running("healthy", None);
        slice.select_last();
        let theme = default_theme();

        // When rendering through the view.
        let terminal = render_at(&slice, 80, 24);

        // Then its State cell carries the dormant token, not the error
        // colour a dormant actor must never borrow.
        assert_eq!(state_colour_on(&terminal, 1), theme.dormant_fg);
    }

    /// Every lifecycle word has a colour and fits the column, and none of
    /// them is a filler. Enumerating the enum here rather than in the
    /// layout test means a new variant cannot slip through untested.
    #[rstest::rstest]
    fn every_lifecycle_has_a_named_word() {
        // Given every lifecycle the runtime can report.
        let theme = default_theme();

        // When reading each one's display word.
        let words: Vec<&str> = [
            crate::ActorLifecycle::Running,
            crate::ActorLifecycle::Idle,
            crate::ActorLifecycle::Escalated,
            crate::ActorLifecycle::Crashed,
        ]
        .into_iter()
        .map(|l| lifecycle_display(l, &theme).0)
        .collect();

        // Then they are four distinct, non-empty names.
        assert_eq!(words.len(), 4);
        for word in &words {
            assert!(!word.is_empty(), "every state names itself");
        }
        let mut sorted = words.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), 4, "no two states share a word: {words:?}");
    }

    /// The Notes column belongs to the owning feature. A row that no
    /// feature has spoken for has an empty cell — the dashboard never
    /// fills it with a lifecycle restatement, which is what made the
    /// census read as a wall of prose about actors doing nothing.
    #[rstest::rstest]
    #[test]
    fn a_runtime_failed_row_with_no_feature_note_shows_no_notes() {
        // Given a crashed actor that no feature has published a note for.
        let mut slice = DashboardState::new();
        slice.mark_failed("worker", crate::ActorLifecycle::Crashed);

        // When rendering through the view.
        let terminal = render_at(&slice, 80, 24);

        // Then the row's text is the state and the name, and nothing else.
        let row = row_text(&terminal, 1);
        assert!(row.contains("Crashed"), "state renders: {row}");
        assert!(row.contains("worker"), "name renders: {row}");
        // And the Notes column is empty: everything after the name
        // column is padding, so the old stop-reason phrases cannot
        // appear there. The state word itself is not the Notes column,
        // hence the exact-column check rather than a substring search.
        let buf = terminal.backend().buffer();
        let notes_x = NAME_X + NAME_COL + COLUMN_SPACING;
        let notes: String = (notes_x..buf.area.width)
            .map(|x| buf[(x, 1)].symbol())
            .collect();
        assert_eq!(notes.trim(), "", "the Notes cell is empty: {notes:?}");
    }

    /// A feature's own status message is the one thing the Notes column
    /// ever shows.
    #[rstest::rstest]
    #[test]
    fn a_features_status_message_renders_in_the_notes_column() {
        // Given a running actor with a feature note.
        let mut slice = DashboardState::new();
        slice.mark_running("discord", None);
        slice.set_status_message("discord", Some("reconnecting".to_owned()));

        // When rendering through the view.
        let terminal = render_at(&slice, 80, 24);

        // Then the note is on screen on that row.
        assert!(row_text(&terminal, 1).contains("reconnecting"));
    }

    /// A feature colours its note by tone, and the tone resolves through
    /// the theme rather than a hardcoded colour.
    #[rstest::rstest]
    #[case(NoteTone::Muted, "muted_text")]
    #[case(NoteTone::Warning, "warning")]
    #[case(NoteTone::Error, "error_text")]
    fn each_note_tone_renders_in_its_theme_token(#[case] tone: NoteTone, #[case] token: &str) {
        // Given an actor whose owning feature published a toned note,
        // alongside a healthy row so the cursor is elsewhere.
        let mut slice = DashboardState::new();
        slice.mark_running("gateway", None);
        slice.set_status_message("gateway", Some("401: invalid bot token".to_owned()));
        slice.set_note_tone("gateway", tone);
        slice.mark_running("healthy", None);
        slice.select_last();
        let theme = default_theme();
        #[expect(
            clippy::unreachable,
            reason = "each #[case] names one of the three tokens above; the arm is a guard for a future case typo"
        )]
        let expected = match token {
            "muted_text" => theme.muted_text,
            "warning" => theme.warning,
            "error_text" => theme.error_text,
            _ => unreachable!("unknown token"),
        };

        // When rendering through the view.
        let terminal = render_at(&slice, 80, 24);

        // Then the note carries that token's colour.
        assert_eq!(note_colour(&terminal, "401:"), expected);
    }

    /// The tone must not leak into the State cell: a feature's opinion
    /// about its own service says nothing about the runtime's verdict on
    /// the actor.
    #[rstest::rstest]
    #[test]
    fn an_error_toned_note_does_not_colour_the_state_cell() {
        // Given a running actor whose feature reported an error, and the
        // cursor parked on a different row so the state cell is readable.
        let mut slice = DashboardState::new();
        slice.mark_running("gateway", None);
        slice.set_status_message("gateway", Some("401: invalid bot token".to_owned()));
        slice.set_note_tone("gateway", NoteTone::Error);
        slice.mark_running("healthy", None);
        slice.select_last();
        let theme = default_theme();

        // When rendering through the view.
        let terminal = render_at(&slice, 80, 24);

        // Then the State cell still reads Running, in the success colour.
        assert_eq!(state_word_on(&terminal, 1), "Running");
        assert_eq!(state_colour_on(&terminal, 1), theme.success);
    }

    #[rstest::rstest]
    #[test]
    fn dashboard_view_renders_empty_placeholder_when_no_actors() {
        // Given an empty dashboard slice.
        let slice = DashboardState::new();
        let theme = default_theme();
        let cx = ViewCx { theme: &theme };

        // When rendering through the view.
        let (mut terminal, _area) = setup_term(80, 24);
        terminal
            .draw(|frame| {
                let mut view = DashboardView::new();
                let area = ratatui::layout::Rect::new(0, 0, 80, 24);
                view.render(frame, area, &cx, &slice);
            })
            .expect("render");

        // Then the placeholder is drawn.
        assert!(buffer_string(&terminal).contains("No services"));
    }

    /// The window is derived from the cursor and the viewport, so a
    /// reader holding only a read handle still draws the selected row
    /// correctly — there is no stored offset left to fall out of date.
    #[rstest::rstest]
    #[test]
    fn the_selected_last_row_renders_without_any_stored_offset() {
        // Given 8 actors with the last selected.
        let mut slice = DashboardState::new();
        for name in ["a", "b", "c", "d", "e", "f", "g", "h"] {
            slice.mark_running(name, None);
        }
        slice.select_last();

        // When rendering into a 5-row-tall viewport (1 header + 4 rows).
        let terminal = render_at(&slice, 80, 5);

        // Then the selected last row is drawn, and the first is not.
        let buf = buffer_string(&terminal);
        assert!(buf.contains('h'), "selected last row renders: {buf}");
        assert!(
            !row_text(&terminal, 1).contains('a'),
            "first row is scrolled out of the window: {}",
            row_text(&terminal, 1)
        );
    }

    /// The cursor renders at the vertical centre of the viewport. This is
    /// the whole point of the derived offset: the reader always sees what
    /// is above and below the row they are on, instead of the list
    /// lurching a row at a time to keep up with `j`.
    #[rstest::rstest]
    #[test]
    fn the_cursor_renders_at_the_vertical_centre_of_the_viewport() {
        // Given 40 actors with the cursor well down the list.
        let mut slice = DashboardState::new();
        for i in 0..40 {
            slice.mark_running(format!("actor-{i:02}"), None);
        }
        for _ in 0..20 {
            slice.select_next();
        }
        assert_eq!(slice.selected_index(), 20);

        // When rendering into a viewport with a header and nine data rows.
        let terminal = render_at(&slice, 80, 10);

        // Then the marker sits on data row five of nine — the middle.
        let cursor = cursor_row(&terminal).expect("a cursor is drawn");
        assert_eq!(cursor, 5, "cursor is centred, buffer row was {cursor}");
        // And the row it landed on is the selected actor.
        assert!(row_text(&terminal, cursor).contains("actor-20"));
    }

    /// At the end of a long list the window pins to the bottom rather
    /// than leaving the cursor floating with empty rows beneath it.
    #[rstest::rstest]
    #[test]
    fn the_end_of_the_list_pins_the_cursor_to_the_bottom_row() {
        // Given 40 actors with the cursor on the last.
        let mut slice = DashboardState::new();
        for i in 0..40 {
            slice.mark_running(format!("actor-{i:02}"), None);
        }
        slice.select_last();

        // When rendering into a viewport with a header and nine data rows.
        let terminal = render_at(&slice, 80, 10);

        // Then the marker sits on the final visible row, and the final
        // actor is on it.
        let cursor = cursor_row(&terminal).expect("a cursor is drawn");
        assert_eq!(cursor, 9, "cursor pinned to the last row, was {cursor}");
        assert!(row_text(&terminal, cursor).contains("actor-39"));
    }

    /// A failed actor sorts to the top of the rendered list, so a reader
    /// opening the tab sees it without scrolling.
    #[rstest::rstest]
    #[test]
    fn a_crashed_actor_renders_at_the_top_of_the_list() {
        // Given a dozen healthy actors and one that failed.
        let mut slice = DashboardState::new();
        for i in 0..12 {
            slice.mark_running(format!("healthy-{i:02}"), None);
        }
        slice.mark_failed("broken", crate::ActorLifecycle::Crashed);

        // When rendering through the view.
        let terminal = render_at(&slice, 80, 24);

        // Then it is the first data row, above every healthy one.
        assert!(row_text(&terminal, 1).contains("broken"));
    }
}

#[cfg(test)]
mod layout_tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing, reason = "test code")]
    use super::*;
    use crate::DashboardState;
    use jinn_slices::view::ViewCx;
    use jinn_testutil::setup_term;
    use jinn_theme::default_theme;

    fn buffer_string(terminal: &ratatui::Terminal<ratatui::backend::TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect()
    }

    /// The width every layout test renders at: wide enough that the
    /// three fixed columns fit with room to spare.
    const WIDTH: u16 = 120;

    /// Renders `slice` at `width` and returns the raw buffer text.
    fn render(slice: &DashboardState, width: u16, height: u16) -> String {
        let theme = default_theme();
        let cx = ViewCx { theme: &theme };
        let (mut terminal, _area) = setup_term(width, height);
        terminal
            .draw(|frame| {
                let mut view = DashboardView::new();
                view.render(frame, Rect::new(0, 0, width, height), &cx, slice);
            })
            .expect("render");
        buffer_string(&terminal)
    }

    /// The longest State word, which is what `STATE_COL` is fitted to.
    ///
    /// Enumerated exhaustively: this is the guard that a new lifecycle
    /// variant gets a real column, and it only bites if the list here is
    /// kept in step with the enum.
    fn longest_state_word() -> u16 {
        [
            ActorLifecycle::Running,
            ActorLifecycle::Idle,
            ActorLifecycle::Escalated,
            ActorLifecycle::Crashed,
        ]
        .into_iter()
        .map(|l| u16::try_from(lifecycle_display(l, &default_theme()).0.width()).unwrap())
        .max()
        .expect("states exist")
    }

    #[rstest::rstest]
    fn the_state_column_is_wide_enough_for_every_state_word() {
        // Given the longest state word the view can render.
        let longest = longest_state_word();

        // When comparing it against the fixed column width.
        // Then the fixed column fits it, with room to spare.
        assert!(
            STATE_COL > longest,
            "STATE_COL {STATE_COL} does not fit the {longest}-cell word {longest} plus padding"
        );
    }

    #[rstest::rstest]
    fn a_name_at_the_column_limit_is_not_truncated() {
        // Given a name exactly the column width.
        let exact = "a".repeat(usize::from(NAME_COL));

        // When measuring it against the column.
        // Then it needs no overlay.
        assert!(!is_truncated(&exact, NAME_COL));
    }

    #[rstest::rstest]
    fn a_name_one_cell_over_the_limit_is_truncated() {
        // Given a name one cell too wide.
        let over = "a".repeat(usize::from(NAME_COL) + 1);

        // When measuring it against the column.
        // Then it does.
        assert!(is_truncated(&over, NAME_COL));
    }

    /// A partition-set entity's real path: `jinn.discovery/<uuid>` is 51
    /// cells, so the common case IS the truncated case.
    #[rstest::rstest]
    fn a_partition_entity_path_overflows_the_name_column() {
        // Given a real derived path.
        let path = "jinn.discovery/0199a3b2-1234-7abc-8def-0123456789ab";

        // When measuring it against the column.
        // Then it overflows and so gets the overlay treatment.
        assert!(path.width() > usize::from(NAME_COL));
        assert!(is_truncated(path, NAME_COL));
    }

    /// Display width, not char count: a CJK name occupies two cells per
    /// character, so 20 characters overflow a 40-cell column while 40 do
    /// not. Measuring bytes would get both wrong.
    #[rstest::rstest]
    fn truncation_is_measured_in_display_cells() {
        // Given a 20-character name that is 40 cells wide, and one that
        // is 42 cells wide.
        let wide = "間".repeat(20);
        let wider = "間".repeat(21);

        // When measuring both in display cells and comparing them to the column.
        // Then the 40-cell one fits and the 42-cell one does not.
        // And the cell count, not the char count, is what drives the decision.
        assert_eq!(wide.chars().count(), 20);
        assert_eq!(wide.width(), 40);
        assert_eq!(wider.width(), 42);
        assert!(!is_truncated(&wide, NAME_COL));
        assert!(is_truncated(&wider, NAME_COL));
    }

    #[rstest::rstest]
    fn the_header_is_state_name_notes() {
        // Given a dashboard with one actor.
        let mut slice = DashboardState::new();
        slice.mark_running("inference", None);

        // When rendering.
        let buf = render(&slice, WIDTH, 10);

        // Then the columns read in the new order.
        // The buffer is read row-major, so the header is the first
        // `width` cells — not the first N characters.
        let header: String = buf.chars().take(usize::from(WIDTH)).collect();
        assert!(header.contains("State"), "header: {header}");
        assert!(header.contains("Name"), "header: {header}");
        assert!(header.contains("Notes"), "header: {header}");
        assert!(
            !header.contains("Description"),
            "the description column is gone: {header}"
        );
    }

    #[rstest::rstest]
    fn a_short_name_renders_in_full_without_an_overlay() {
        // Given a short-named actor that is selected.
        let mut slice = DashboardState::new();
        slice.mark_running("inference", None);
        slice.select_first();

        // When rendering.
        let buf = render(&slice, WIDTH, 10);

        // Then the name appears exactly as stored.
        assert!(buf.contains("inference"), "short name renders: {buf}");
    }

    #[rstest::rstest]
    fn a_long_name_on_the_selected_row_overlays_in_full() {
        // Given a selected actor whose name overflows the column.
        let mut slice = DashboardState::new();
        let long = "jinn.discovery/0199a3b2-1234-7abc-8def-0123456789ab";
        slice.mark_running(long, None);
        slice.select_first();

        // When rendering.
        let buf = render(&slice, 120, 10);

        // Then the FULL name is on screen, not the clipped prefix — this
        // is the whole point of the overlay.
        assert!(buf.contains(long), "the full name overlays: {buf}");
    }

    /// Without the overlay a long name is clipped by the column, so the
    /// tail is absent. This pins that the overlay is what makes it
    /// visible, rather than the name happening to fit.
    #[rstest::rstest]
    fn the_overlay_is_what_makes_a_long_name_readable() {
        // Given the same long-named actor, NOT selected.
        let mut slice = DashboardState::new();
        let long = "jinn.discovery/0199a3b2-1234-7abc-8def-0123456789ab";
        slice.mark_running(long, None);
        let other = "inference";
        slice.mark_running(other, None);
        slice.select_next(); // select `inference`, leaving the long row unselected

        // When rendering.
        let buf = render(&slice, 120, 10);

        // Then the long name is clipped (tail absent) and the selected
        // short one is intact.
        assert!(
            !buf.contains("0123456789ab"),
            "an unselected long name stays clipped: {buf}"
        );
        assert!(
            buf.contains(other),
            "the selected short name renders: {buf}"
        );
    }

    /// The overlay is only correct if it lands on the selected row. With
    /// two long names, only the selected one's full value may appear.
    #[rstest::rstest]
    fn only_the_selected_long_name_is_overlaid() {
        // Given two long-named actors, the second selected.
        let mut slice = DashboardState::new();
        let first = "jinn.discovery/aaaaaaaa-aaaa-7aaa-8aaa-aaaaaaaaaaaa";
        let second = "jinn.mcp.coordinator/12345678-1234-7abc-8def-abcdefabcdef";
        slice.mark_running(first, None);
        slice.mark_running(second, None);
        slice.select_next();

        // When rendering.
        let buf = render(&slice, 140, 10);

        // Then the selected one is whole and the unselected one is not.
        assert!(buf.contains(second), "selected name overlays: {buf}");
        assert!(
            !buf.contains("aaaaaaaaaaaa"),
            "an unselected long name stays clipped: {buf}"
        );
    }

    /// The overlay paints over Notes for the selected row. That is the
    /// accepted cost, but it must not touch any OTHER row.
    #[rstest::rstest]
    fn the_overlay_covers_only_its_own_row() {
        // Given a long-named selected row and a short row with notes.
        let mut slice = DashboardState::new();
        let long = "jinn.discovery/0199a3b2-1234-7abc-8def-0123456789ab";
        slice.mark_running(long, None);
        slice.set_status_message(long, Some("first-row-note".to_owned()));
        slice.mark_running("inference", None);
        slice.set_status_message("inference", Some("second-row-note".to_owned()));
        slice.select_first();

        // When rendering.
        let buf = render(&slice, 140, 10);

        // Then the other row's notes survive intact.
        assert!(
            buf.contains("second-row-note"),
            "the overlay must not reach the next row: {buf}"
        );
    }

    #[rstest::rstest]
    fn the_overlay_follows_the_derived_scroll_offset() {
        // Given many actors so the list scrolls, with the selected one
        // far down the list.
        let mut slice = DashboardState::new();
        for i in 0..40 {
            slice.mark_running(format!("actor-{i}"), None);
        }
        let long = "jinn.discovery/0199a3b2-1234-7abc-8def-0123456789ab";
        slice.mark_running(long, None);
        slice.select_last();

        // When rendering into a short viewport.
        let buf = render(&slice, 140, 6);

        // Then the selected long name is still overlaid, because it is
        // scrolled INTO view rather than out of it.
        assert!(
            buf.contains(long),
            "the overlay tracks the scrolled row: {buf}"
        );
    }

    #[rstest::rstest]
    fn a_name_column_of_the_configured_width_leaves_the_rest_to_notes() {
        // Given a row with a long note.
        let mut slice = DashboardState::new();
        slice.mark_running("inference", None);
        slice.set_status_message("inference", Some("a-fairly-long-status-phrase".to_owned()));

        // When rendering at a wide terminal.
        let buf = render(&slice, 120, 10);

        // Then the note is drawn, not squeezed out by a fixed name width.
        assert!(
            buf.contains("a-fairly-long-status-phrase"),
            "notes absorb the remainder: {buf}"
        );
    }

    #[rstest::rstest]
    fn row_y_accounts_for_the_header_and_the_offset() {
        // Given a 10-row area and a 3-row scroll offset.
        let area = Rect::new(0, 0, 80, 10);

        // When mapping each row to its screen line.
        // Then the first visible data row lands under the header.
        assert_eq!(row_y(area, 3, 3), Some(1));
        assert_eq!(row_y(area, 4, 3), Some(2));
        // And a row scrolled out of view has nowhere to draw.
        assert_eq!(row_y(area, 2, 3), None);
        // And a row past the bottom of the viewport is out too.
        assert_eq!(row_y(area, 20, 0), None);
    }

    /// The overlay must be INVISIBLE as a seam: its text carries the same
    /// foreground the table gives the selected row, so the revealed tail
    /// reads as part of the highlighted cursor row.
    #[rstest::rstest]
    fn the_overlay_text_uses_the_cursor_row_highlight() {
        // Given a selected long-named actor.
        let mut slice = DashboardState::new();
        let long = "jinn.discovery/0199a3b2-1234-7abc-8def-0123456789ab";
        slice.mark_running(long, None);
        slice.select_first();

        // When rendering.
        let theme = default_theme();
        let cx = ViewCx { theme: &theme };
        let (mut terminal, _area) = setup_term(WIDTH, 8);
        terminal
            .draw(|frame| {
                let mut view = DashboardView::new();
                view.render(frame, Rect::new(0, 0, WIDTH, 8), &cx, &slice);
            })
            .expect("render");

        // Then a cell beyond the 40-column name limit — the revealed tail,
        // which no table cell covers — carries the highlight colour, not
        // the neutral name colour.
        let buf = terminal.backend().buffer();
        let y = 1; // the selected data row
        let x = NAME_X + NAME_COL;
        assert!(
            x < WIDTH,
            "the tail must be on screen for this to mean anything"
        );
        assert_eq!(buf[(x, y)].fg, theme.focus_accent, "tail is highlighted");
        assert_ne!(
            buf[(x, y)].fg,
            theme.primary_text,
            "the overlay must not use the neutral name colour"
        );
    }

    /// The highlight bar must run to the end of the row. `Clear` resets
    /// styling, so without explicit padding the row would look highlighted
    /// only as far as the name.
    #[rstest::rstest]
    fn the_overlay_keeps_the_highlight_running_to_the_end_of_the_row() {
        // Given a selected long-named actor.
        let mut slice = DashboardState::new();
        let long = "jinn.discovery/0199a3b2-1234-7abc-8def-0123456789ab";
        slice.mark_running(long, None);
        slice.select_first();

        // When rendering.
        let theme = default_theme();
        let cx = ViewCx { theme: &theme };
        let (mut terminal, _area) = setup_term(WIDTH, 8);
        terminal
            .draw(|frame| {
                let mut view = DashboardView::new();
                view.render(frame, Rect::new(0, 0, WIDTH, 8), &cx, &slice);
            })
            .expect("render");

        // Then the last cell of the row is still highlighted, not reset.
        let buf = terminal.backend().buffer();
        let y = 1;
        let last = WIDTH - 1;
        assert_eq!(
            buf[(last, y)].fg,
            theme.focus_accent,
            "the highlight bar reaches the row's end"
        );
    }

    /// The overlay covers that row's Notes. That is the accepted cost of
    /// breaking the column width, pinned so a future change to it is
    /// deliberate.
    #[rstest::rstest]
    fn the_overlay_covers_the_selected_rows_own_notes() {
        // Given a selected long-named row that has notes.
        let mut slice = DashboardState::new();
        let long = "jinn.discovery/0199a3b2-1234-7abc-8def-0123456789ab";
        slice.mark_running(long, None);
        slice.set_status_message(long, Some("a-note-that-should-be-covered".to_owned()));
        slice.select_first();

        // When rendering.
        let buf = render(&slice, WIDTH, 8);

        // Then the note is not visible under the overlay.
        assert!(
            !buf.contains("a-note-that-should-be-covered"),
            "the overlay covers its own row's notes: {buf}"
        );
    }

    /// The real geometry: the app hands the dashboard the content region
    /// BELOW the tab bar, so the area starts at y=1, not y=0.
    ///
    /// Every other overlay test renders at the buffer origin, which is
    /// why an overlay drawn one row HIGH went unnoticed: relative and
    /// absolute rows coincide at y=0 and only diverge below it. This is
    /// the test that pins the overlay to the cursor row in the layout
    /// the app actually uses.
    #[rstest::rstest]
    fn the_overlay_lands_on_the_cursor_row_not_the_one_above() {
        // Given a content area that starts below a 1-row tab bar.
        let area = Rect::new(0, 1, WIDTH, 8);
        let mut slice = DashboardState::new();
        let long = "jinn.discovery/0199a3b2-1234-7abc-8def-0123456789ab";
        slice.mark_running(long, None);
        slice.mark_running("inference", None);
        slice.select_first();

        // When rendering.
        let theme = default_theme();
        let cx = ViewCx { theme: &theme };
        let (mut terminal, _area) = setup_term(WIDTH, 9);
        terminal
            .draw(|frame| {
                let mut view = DashboardView::new();
                view.render(frame, area, &cx, &slice);
            })
            .expect("render");

        // Then the header (the first row of the area, buffer row 1) is
        // untouched, and the revealed tail sits on the FIRST DATA row
        // (buffer row 2) — the same row as the cursor marker.
        let buf = terminal.backend().buffer();
        let tail_x = NAME_X + NAME_COL;
        assert_eq!(
            buf[(tail_x, 2)].fg,
            theme.focus_accent,
            "the tail is highlighted on the data row"
        );
        assert_ne!(
            buf[(tail_x, 1)].fg,
            theme.focus_accent,
            "the header row must not be overwritten by the overlay"
        );
        // And the full name is on the data row, not above it.
        let row_text = |y: u16| (0..WIDTH).map(|x| buf[(x, y)].symbol()).collect::<String>();
        assert!(
            row_text(2).contains(long),
            "the full name is on the cursor row: {}",
            row_text(2)
        );
        assert!(
            !row_text(1).contains(long),
            "and not on the row above: {}",
            row_text(1)
        );
    }

    /// The offset of the content area must not shift the name column
    /// horizontally either — the overlay starts at the name column, not
    /// at the buffer's left edge.
    #[rstest::rstest]
    fn the_overlay_starts_at_the_name_column_of_an_offset_area() {
        // Given a content area with a non-zero x and y.
        let area = Rect::new(4, 2, WIDTH, 8);
        let mut slice = DashboardState::new();
        let long = "jinn.discovery/0199a3b2-1234-7abc-8def-0123456789ab";
        slice.mark_running(long, None);
        slice.select_first();

        // When rendering.
        let theme = default_theme();
        let cx = ViewCx { theme: &theme };
        let (mut terminal, _area) = setup_term(WIDTH + 4, 10);
        terminal
            .draw(|frame| {
                let mut view = DashboardView::new();
                view.render(frame, area, &cx, &slice);
            })
            .expect("render");

        // Then the name begins at the area's x plus the name column.
        let buf = terminal.backend().buffer();
        let row = (0..WIDTH + 4)
            .map(|x| buf[(x, 3)].symbol())
            .collect::<String>();
        let name_column = usize::from(area.x + NAME_X);
        {
            let (_, after_name_column) = row
                .split_at_checked(name_column)
                .expect("the row is WIDTH+4 wide and the name column sits inside it");
            assert!(
                after_name_column.contains(long),
                "the name starts at the column, not the area edge: {row}"
            );
        }
    }

    #[rstest::rstest]
    fn absolute_y_shifts_by_the_area_origin_and_rejects_the_bottom_edge() {
        // Given an area starting below the tab bar.
        let area = Rect::new(0, 1, 80, 10);

        // When mapping each row to an absolute screen line.
        // Then a relative row is shifted by the origin.
        assert_eq!(absolute_y(area, 0), Some(1));
        assert_eq!(absolute_y(area, 1), Some(2));
        // And the area's LAST row is still inside it: rows 1..=10.
        assert_eq!(absolute_y(area, 9), Some(10));
        // And anything past that bottom edge is rejected.
        assert_eq!(absolute_y(area, 10), None);
        assert_eq!(absolute_y(area, 100), None);
        // And an origin-anchored area is the identity.
        assert_eq!(absolute_y(Rect::new(0, 0, 80, 10), 3), Some(3));
    }

    #[rstest::rstest]
    fn the_name_column_starts_after_the_highlight_state_and_spacing() {
        // Given the fixed column geometry.
        // When deriving the name column from the highlight, state, and gap.
        // Then the name begins past the highlight symbol, state, and gap.
        assert_eq!(NAME_X, HIGHLIGHT + STATE_COL + COLUMN_SPACING);
        // And it starts on screen in a normal-width terminal.
        const {
            assert!(NAME_X < 80, "the name column fits a normal terminal");
        }
    }
}
