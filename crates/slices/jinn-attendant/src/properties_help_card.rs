//! The help card — the panel that explains the focused field.
//!
//! Pressing `?` on any row of the attendant properties form opens this: a
//! bordered card naming the focused field and describing what it accepts,
//! with its selectable words in the same color the form itself uses so the two
//! agree on which words are pickable and which are explanation.
//!
//! Separate from [`super::properties_overlay`] because it is a different widget
//! with a different job. The form renders a fixed-height grid of fields; this
//! renders a variable-height document that has to be measured, wrapped, and
//! placed against the form's own bounds — and it is the only thing in the slice
//! that does any of that. It reads the form's state and draws; it never writes.

use jinn_attendant_msg::{AttendantPropertiesState, PropertyField};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

/// The help card's body, for the focused field.
///
/// The body is a [`Line`], not a string: a field's help is a list of
/// choices or a template, and a list reads as a list. Wrapping one choice
/// per row also puts each name where a reader expects it, instead of
/// leaving them mid-sentence on whatever row the wrap happened to break.
///
/// The text is plain on purpose. The card is a [`Paragraph`] and its
/// construction is the place to add emphasis — see [`help_paragraph`].
fn help_body(field: PropertyField, theme: &jinn_theme::Theme) -> Vec<Line<'static>> {
    // A choice's name is the same green the field's own selected choice
    // wears, so the card and the form agree on which words are selectable
    // and which are explanation.
    let choice = Style::default().fg(theme.attendant_option_active);
    let said = Style::default().fg(theme.primary_text);
    let line = |name: &str, description: &str| {
        Line::from(vec![
            Span::raw("  "),
            Span::styled(name.to_owned(), choice),
            Span::styled(format!(":  {description}"), said),
        ])
    };
    match field {
        PropertyField::Trigger => vec![
            Line::from("Condition to trigger the attendant."),
            Line::from(""),
            line("parent-completed", "runs when the agent finishes it's turn"),
            line("manual", "only the `R` keybind will run the attendant"),
        ],
        PropertyField::Behavior => vec![
            Line::from("How to manage the context when the attendant is triggered."),
            Line::from(""),
            line("reset", "pins are kept, all other context excluded/hidden"),
            line("preserve", "one continuous chat session; no context edits"),
        ],
        PropertyField::PrepMode => vec![
            Line::from(
                "Attendants need to be 'prepped' before usage by providing pinned context to define their behavior. Messages land in context pinned and not sent to a provider.",
            ),
            Line::from(""),
            line(
                "on",
                "prepare the session by submitting messages; attendant disabled",
            ),
            line("off", "attendant is active"),
        ],
        PropertyField::ToolSet => vec![
            Line::from(
                "Controls the working tool set and how tool enablement works. If you manually configured globs in the config file, then they will be dropped upon switching modes (their meaning will invert)",
            ),
            Line::from(""),
            line(
                "live",
                "(block list) - Tools that were disabled for the attendant stay disabled. New tools are automatically enabled, mirroring jinn's enabled-by-default behavior",
            ),
            line(
                "frozen",
                "(allow list) - The tools enabled for the attendant are the only ones available. New tools are automatically disabled.",
            ),
        ],
        PropertyField::SkillSet => vec![
            Line::from(
                "Controls the working skill set and how skill enablement works. If you manually configured globs in the config file, then they will be dropped upon switching modes (their meaning will invert).",
            ),
            Line::from(""),
            line(
                "live",
                "(block list) - Skills that were disabled for the attendant stay disabled. New skills are automatically enabled, mirroring jinn's enabled-by-default behavior",
            ),
            line(
                "frozen",
                "(allow list) - The skills enabled for the attendant are the only ones available. New skills are automatically disabled.",
            ),
        ],
        PropertyField::Model => vec![
            Line::from(
                "Whether to use the model in the parent session at spawn time, or one saved in the attendent config.",
            ),
            Line::from(""),
            line(
                "inherit",
                "the model is the one this attendant was created with",
            ),
            line(
                "fixed",
                "the model selected in the attendant session will persist on all new attachments",
            ),
        ],
        PropertyField::SeedTemplate => vec![
            Line::from(
                "Text injected on each run, ahead of the previous report. Useful when using `reset` behavior to advise the agent of the previous conclusion.",
            ),
            Line::from(""),
            line(
                "<prior report>",
                "replacement token you can optionally add to the see template",
            ),
        ],
    }
}

/// The cells the help card's border takes off its own width: one on each
/// side. The text is drawn inside them.
const HELP_CARD_BORDER_CELLS: u16 = 2;

/// The help card's text: a header naming the field, then its body lines.
///
/// Every row carries the card's own background, and its foreground is
/// chosen by the row's *role* — header, body — so adding emphasis means
/// giving a line a color, not restating the surface it sits on. The body
/// lines are wrapped here rather than handed to the `Paragraph`, because the
/// card sizes itself to its content and must know its height before it can
/// be placed.
fn help_body_lines(
    field: PropertyField,
    inner_width: u16,
    theme: &jinn_theme::Theme,
) -> Vec<Line<'static>> {
    let text_width = inner_width.max(1);
    let mut lines = vec![Line::from(Span::styled(
        // The label as the form shows it, minus the colon padding: the
        // card is a description of the field, not a copy of its row.
        field.label().to_owned(),
        Style::default().fg(theme.focus_accent),
    ))];
    lines.extend(
        help_body(field, theme)
            .into_iter()
            .flat_map(|line| wrap_line(line, text_width))
            // `Line::style` replaces rather than patches, so the surface is
            // restated on every row: a body's own colors layered on a card
            // background would keep the foreground and drop the surface.
            .map(|line| line.style(Style::default().bg(theme.user_block_bg))),
    );
    // A blank line in the authored body is the author's own spacing and is
    // kept as authored; the card adds no pad of its own, so its height is
    // exactly its text.
    lines
}

/// The help card's frame: the attendant's own pink around its text.
///
/// The border is the card's identity — it says *this box is about the
/// attendant's settings* the moment it appears, before a word of it is
/// read, and it keeps the card from bleeding into whatever the chat log
/// happened to have behind it. It is [`attendant_fg`] rather than a new
/// color because that pink is already the app's word for "attendant".
///
/// The block carries the card's surface as well as its edge, for the same
/// reason the rows do: a `Paragraph` paints only the cells a line's text
/// covers, and a card with a blank row in it would otherwise show a hole
/// in its own background rather than a blank row of card.
fn help_card(theme: &jinn_theme::Theme) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(
            Style::default()
                .fg(theme.attendant_fg)
                .bg(theme.user_block_bg),
        )
        .style(Style::default().bg(theme.user_block_bg))
}

/// Wraps one help line to `width`, breaking on whitespace, and returns the
/// rows it occupies.
///
/// A word too long for a row of its own is placed on the next row rather
/// than split mid-word; that is the only case that can overflow, and it is
/// better one long row than a word broken across two. An empty line stays
/// exactly one row: it is the blank the authored help asked for.
fn wrap_line(line: Line<'static>, width: u16) -> Vec<Line<'static>> {
    let width = usize::from(width.max(1));
    if line.spans.iter().all(|span| span.content.is_empty()) {
        // A line of nothing: it asked for a row, and wrapping it to no rows
        // at all would drop the author's blank.
        return vec![line];
    }
    // Words are separated by their *authored* gap, and a run of whitespace is
    // a run: the help lines are aligned columns, and collapsing the gap
    // between a name and its description to one space would throw away the
    // only thing that makes the list a list.
    let mut rows: Vec<Line<'static>> = Vec::new();
    let mut current: Vec<Span<'static>> = Vec::new();
    let mut used = 0usize;
    for span in line.spans {
        // The whitespace between the span's words, as authored, and a row
        // edge: an authored gap cannot be measured before the words around
        // it are known, so it is buffered here rather than emitted.
        let mut gap = String::new();
        let mut at_row_start = true;
        for token in span.content.split(' ') {
            if token.is_empty() {
                // A run of spaces: the run's own length is the gap, and it
                // collapses to the count that fits.
                if !at_row_start {
                    gap.push(' ');
                }
                continue;
            }
            let length = token.chars().count();
            let separator = usize::min(gap.len(), width.saturating_sub(1));
            if used + separator + length > width {
                rows.push(Line::from(std::mem::take(&mut current)));
                used = 0;
            } else if separator > 0 {
                current.push(Span::styled(" ".repeat(separator), span.style));
                used += separator;
            } else {
                // No room for a gap: the token starts the row alone.
            }
            current.push(Span::styled(token.to_owned(), span.style));
            used += length;
            gap.clear();
            gap.push(' ');
            at_row_start = false;
        }
    }
    if !current.is_empty() {
        rows.push(Line::from(current));
    }
    if rows.is_empty() {
        rows.push(Line::default());
    }
    rows
}

/// The help overlay for the focused field, or `None` when it is not showing.
///
/// The card is drawn below the popup, at the popup's own width, with the
/// text wrapped to it. It is an overlay on the terminal rather than a row
/// inside the form, and it sizes itself to its content, so its height is
/// measured before it is placed.
pub(super) fn help_overlay<'a>(
    popup: &AttendantPropertiesState,
    popup_area: Rect,
    inner: Rect,
    terminal: Rect,
    theme: &'a jinn_theme::Theme,
) -> Option<HelpCard<'a>> {
    // The template editor owns the terminal cursor while it is open; the
    // help would sit on top of the draft the user is typing into.
    if !popup.help_visible || popup.editor_original.is_some() {
        return None;
    }
    let card = help_card(theme);
    // The text is drawn inside the border, so it wraps a card narrower and
    // rows shorter than the card. Wrapping at the card's own width instead
    // would under-count the rows, and a card that is shorter than its text
    // runs off its own bottom border.
    let text_width = inner.width.saturating_sub(HELP_CARD_BORDER_CELLS);
    let lines = help_body_lines(popup.focus, text_width, theme);
    // The card is a border around its text, so its height is the text's
    // plus the two border rows — measured here rather than left to the
    // `Block`, because the card is placed before it is drawn and a height
    // that is only known at draw time is a height that cannot be placed.
    let height = u16::try_from(lines.len())
        .unwrap_or(u16::MAX)
        .saturating_add(2)
        .max(2);
    let y = place_help(&HelpPlacementInput {
        height,
        // The card is kept off the popup *including its border*, not just
        // its body: a card drawn over the popup's own top edge reads as a
        // rendering fault rather than as an overlay.
        popup_bottom: popup_area.y.saturating_add(popup_area.height),
        terminal_top: terminal.y,
        terminal_bottom: terminal.y.saturating_add(terminal.height),
    });
    let area = Rect {
        x: inner.x,
        y,
        width: inner.width,
        height,
    };
    let text = card.inner(area);
    Some(HelpCard {
        area,
        text,
        card,
        lines,
    })
}

/// The help card, placed: the area it occupies, the text area inside its
/// border, the frame that draws the border, and the lines the text is.
pub(super) struct HelpCard<'a> {
    /// The card's whole area, border included.
    area: Rect,
    /// The area inside the border, where the text goes.
    text: Rect,
    /// The card's frame.
    card: Block<'a>,
    /// The card's text, already wrapped to `text`'s width.
    lines: Vec<Line<'static>>,
}

/// Where the help card goes: its height, and the bounds it must fit in.
struct HelpPlacementInput {
    /// The card's measured height.
    height: u16,
    /// One past the popup's last row, border included.
    popup_bottom: u16,
    /// The terminal's first row.
    terminal_top: u16,
    /// One past the terminal's last row.
    terminal_bottom: u16,
}

/// The card's top row: always below the popup, flush with its border.
///
/// Below is the card's only home. Above was the old placement, and a card
/// that flips sides as the user moves between fields is a card that jumps
/// under the cursor — the help is read, not aimed at, and a fixed place is
/// what lets a reader find the next field's help without hunting for it.
///
/// The card sits on the popup's last row. There was a blank row between
/// them once, to keep the two from reading as one block; the card's own
/// pink border does that job, and does it better than an empty row — a gap
/// between two framed things is a hole in the screen, not separation. A
/// terminal too short to hold the card below it draws from its own top row
/// and lets the last rows be cut: an overlay is laid over the screen, and
/// there is nowhere else to put it.
fn place_help(input: &HelpPlacementInput) -> u16 {
    let below = input
        .popup_bottom
        .min(input.terminal_bottom.saturating_sub(input.height));
    if below.saturating_add(input.height) <= input.terminal_bottom {
        below
    } else {
        // No room below. The card is drawn from the top of the terminal and
        // the terminal cuts it, rather than from the top of the room below
        // the popup: a card that starts above the popup covers the form it
        // is describing. Neither reads as an overlay.
        input.terminal_top
    }
}

impl HelpCard<'_> {
    /// Draws the card: clear its area, lay the border, then the text inside it.
    ///
    /// The border goes down before the text so it paints the card's surface;
    /// the text then goes into the area the border leaves. Doing this here
    /// rather than at the call site keeps the card's parts private — a caller
    /// that reached for `area` and `card` separately would have to know the
    /// order they compose in.
    pub(super) fn draw(self, frame: &mut Frame<'_>) {
        frame.render_widget(Clear, self.area);
        frame.render_widget(self.card, self.area);
        frame.render_widget(Paragraph::new(self.lines), self.text);
    }
}
