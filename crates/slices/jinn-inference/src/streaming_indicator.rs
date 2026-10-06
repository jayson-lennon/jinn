//! Streaming indicator element with animated throbber.
//!
//! Renders an animated ASCII spinner alongside "Working..." when the active
//! session is busy (sending, streaming, or compacting), and renders nothing
//! when idle. Queue count is shown when messages are waiting (not during
//! compaction).
//!
//! The row also names what kind of session is on screen. A subagent or an
//! attendant is a conversation with its own identity, and the chat pane has no
//! title bar — the session list's colors are the only other place that says so,
//! and they are off-screen while the user reads. So the row's right edge carries
//! the kind in that kind's color at all times, busy or not.

use std::time::Instant;

use jinn_kernel::common::app_state::AppState;
use jinn_kernel::common::render_ctx::RenderCtx;
use jinn_kernel::common::ui_element::UiElement;
use jinn_session_msg::{PhaseKind, SessionOrigin};
use jinn_slices::DrawContext;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use throbber_widgets_tui::{Throbber, ThrobberState, WhichUse};

/// Displays an animated streaming indicator when the active session is sending, streaming, or compacting.
#[derive(Debug)]
pub struct StreamingIndicatorElement {
    /// Visual-only state for the throbber animation step.
    throbber_state: ThrobberState,
    /// Timestamp of the last animation frame advance.
    last_animation_step: Instant,
}

impl StreamingIndicatorElement {
    /// Creates a new streaming indicator element.
    pub fn new() -> Self {
        Self {
            throbber_state: ThrobberState::default(),
            last_animation_step: Instant::now(),
        }
    }

    /// Advances the animation frame if enough time has elapsed.
    fn maybe_advance_animation(&mut self) {
        if self.last_animation_step.elapsed() >= jinn_slices::SPINNER_INTERVAL {
            self.throbber_state.calc_next();
            self.last_animation_step = Instant::now();
        }
    }
}

impl Default for StreamingIndicatorElement {
    fn default() -> Self {
        Self::new()
    }
}

/// Paints the streaming indicator into `area`.
///
/// The indicator holds throbber animation state, so the registered
/// draw function keeps exactly one element behind interior mutability
/// and the animation advances across frames.
pub fn paint(
    element: &mut StreamingIndicatorElement,
    frame: &mut Frame<'_>,
    area: Rect,
    ctx: &dyn DrawContext<AppState>,
) {
    element.render_body(frame, area, ctx.state());
}

impl StreamingIndicatorElement {
    /// The indicator's draw body, without the [`UiElement`] plumbing.
    fn render_body(&mut self, frame: &mut Frame<'_>, area: Rect, state: &AppState) {
        let session = state.active_session();
        let phase = session.phase();
        let origin = session.origin();

        let is_phase_busy = matches!(phase, PhaseKind::Sending | PhaseKind::Streaming);
        let is_spinning = is_phase_busy;

        let kind = KindLabel::of(
            origin,
            state.frontend.theme.attendant_fg,
            state.frontend.theme.subagent_fg,
        );
        // Nothing to draw is the only reason to leave the row untouched: an idle
        // user session is the sole case where the row is blank.
        if !is_spinning && kind.is_none() {
            return;
        }

        // The spinner owns the row's left edge and the kind label its right;
        // `indicator_row` places both. The throbber becomes a `Line` rather than
        // being rendered as a widget, because a widget takes the whole area it
        // is given and there is no way to hold a second element at its right
        // edge. `to_line` reads the same state the widget's render would, so
        // the glyph still steps every frame.
        let busy = is_spinning.then(|| {
            let text = if matches!(phase, PhaseKind::Sending) {
                " Working..."
            } else {
                " Streaming..."
            };
            let style = Style::default().fg(state.frontend.theme.streaming);
            let throbber = Throbber::default()
                .label(Span::styled(text, style))
                .style(style)
                .throbber_style(style)
                .throbber_set(throbber_widgets_tui::ASCII)
                .use_type(WhichUse::Spin);
            // Normalized before it is read so the stored step stays in range
            // across a long idle stretch rather than drifting until the widget
            // has to clamp it back.
            self.throbber_state.normalize(&throbber);
            throbber.to_line(&self.throbber_state)
        });

        frame.render_widget(indicator_row(busy, kind, area), area);

        // Advance the animation step only when enough time has elapsed, and
        // only while there is a glyph to advance. The gate is load-bearing now
        // that a label alone can get the row past the early return: an idle
        // attendant session draws every frame and must not spend its idle time
        // spinning up a spinner it never shows.
        if is_spinning {
            self.maybe_advance_animation();
        }
    }
}

/// The kind of conversation on screen, when it is one the reader cannot infer
/// from the chat pane itself.
///
/// A user or fork session is an ordinary conversation and says nothing. A
/// subagent or an attendant is a conversation the user did not type into — it
/// was opened for them — so the row names it. Origin is the whole test: a fork
/// of a subagent session is a plain conversation again, and reads as one.
#[derive(Debug, Clone, Copy)]
enum KindLabel {
    /// An attendant's own conversation.
    Attendant(Color),
    /// A subagent's conversation.
    Subagent(Color),
}

impl KindLabel {
    /// The label for `origin`, or `None` for a kind the reader can already see
    /// is an ordinary conversation.
    fn of(origin: SessionOrigin, attendant_fg: Color, subagent_fg: Color) -> Option<Self> {
        match origin {
            SessionOrigin::Attendant => Some(Self::Attendant(attendant_fg)),
            SessionOrigin::Subagent => Some(Self::Subagent(subagent_fg)),
            SessionOrigin::User | SessionOrigin::Fork => None,
        }
    }

    /// The bracketed text the row shows.
    fn text(self) -> &'static str {
        match self {
            Self::Attendant(_) => "[attendant]",
            Self::Subagent(_) => "[subagent]",
        }
    }

    /// The color the session list already uses for this kind, so the two places
    /// that name the same kind never disagree.
    fn style(self) -> Style {
        let color = match self {
            Self::Attendant(color) | Self::Subagent(color) => color,
        };
        Style::default().fg(color)
    }
}

/// The row's spans: the busy indicator at the left, the kind label at the
/// right, and the gap between them that holds them there.
///
/// A row too narrow for both keeps the label and drops the busy text: the
/// label is the fact about the conversation and stays true across the whole
/// session, where the busy text only describes this instant.
fn indicator_row(busy: Option<Line<'_>>, kind: Option<KindLabel>, area: Rect) -> Line<'_> {
    let mut spans = Vec::new();
    if let Some(busy) = busy {
        spans.extend(busy.spans);
    }
    if let Some(kind) = kind {
        let label = Line::from(Span::styled(kind.text(), kind.style()));
        if let Some(gap) = gap_before(spans_width(&spans), label.width(), area.width) {
            spans.push(Span::raw(" ".repeat(gap)));
        }
        spans.extend(label.spans);
    }
    Line::from(spans)
}

/// The columns between what has been written at the left and `label_width`
/// columns of label, or `None` when the row is too narrow to hold both.
///
/// `None` is the answer rather than a zero gap because a zero gap would butt
/// the label against the busy text, which reads as one run-on string rather than
/// as two facts.
fn gap_before(used: usize, label_width: usize, row_width: u16) -> Option<usize> {
    let room = usize::from(row_width)
        .checked_sub(used)?
        .checked_sub(label_width)?;
    (room > 0).then_some(room)
}

/// The columns `spans` occupy.
fn spans_width(spans: &[Span<'_>]) -> usize {
    spans.iter().map(Span::width).sum()
}

impl UiElement for StreamingIndicatorElement {
    fn name(&self) -> String {
        "streaming-indicator".to_owned()
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx) {
        self.render_body(frame, area, ctx.state);
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]
    use jinn_kernel::AppState;

    use super::*;

    #[rstest::rstest]
    fn name_returns_streaming_indicator() {
        // Given a StreamingIndicatorElement.
        let element = StreamingIndicatorElement::new();

        // When querying the name.
        let name = element.name();

        // Then it is "streaming-indicator".
        assert_eq!(name, "streaming-indicator");
    }

    #[rstest::rstest]
    fn renders_working_label_during_sending_phase() {
        // Given a session in Sending phase — a turn waiting on its tool
        // loop, which is work the user watches.
        use jinn_testutil::{buffer_row, setup_term};

        let mut element = StreamingIndicatorElement::new();
        let mut state = AppState::default();
        state.active_session_mut().begin_sending();
        let (mut terminal, area) = setup_term(30, 1);

        // When rendering the element.
        terminal
            .draw(|frame| {
                let slices = jinn_slices::Slices::new();
                let overlay_views = jinn_slices::OverlayViews::new();
                let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
                element.render(frame, area, &ctx);
            })
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let row = buffer_row(&buffer, 0, 30);

        // Then the label shows "Working...".
        assert!(
            row.contains("Working..."),
            "expected Working..., got: {row}"
        );
    }

    #[rstest::rstest]
    fn renders_streaming_label_during_streaming_phase() {
        // Given a session in Streaming phase.
        use jinn_testutil::{buffer_row, setup_term};

        let mut element = StreamingIndicatorElement::new();
        let mut state = AppState::default();
        state.active_session_mut().begin_streaming();
        let (mut terminal, area) = setup_term(30, 1);

        // When rendering the element.
        terminal
            .draw(|frame| {
                let slices = jinn_slices::Slices::new();
                let overlay_views = jinn_slices::OverlayViews::new();
                let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
                element.render(frame, area, &ctx);
            })
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let row = buffer_row(&buffer, 0, 30);

        // Then the label shows "Streaming...".
        assert!(
            row.contains("Streaming..."),
            "expected Streaming..., got: {row}"
        );
    }

    #[rstest::rstest]
    fn does_not_render_during_idle_phase() {
        // Given a session in Idle phase (default).
        use jinn_testutil::setup_term;

        let mut element = StreamingIndicatorElement::new();
        let state = AppState::default();
        let (mut terminal, area) = setup_term(30, 1);

        // When rendering the element.
        terminal
            .draw(|frame| {
                let slices = jinn_slices::Slices::new();
                let overlay_views = jinn_slices::OverlayViews::new();
                let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
                element.render(frame, area, &ctx);
            })
            .unwrap();
        let buffer = terminal.backend().buffer().clone();

        // Then the rendered area is empty (all spaces).
        let content: String = (0..30)
            .filter_map(|x| buffer.cell((x, 0)).map(ratatui::buffer::Cell::symbol))
            .collect();
        assert!(
            content.trim().is_empty(),
            "expected empty buffer, got: {content}"
        );
    }

    #[rstest::rstest]
    fn renders_working_during_sending_phase() {
        // Given a session in the Sending phase — the tool-loop wait, where
        // "working" is what the user watches.
        use jinn_testutil::{buffer_row, setup_term};

        let mut element = StreamingIndicatorElement::new();
        let mut state = AppState::default();
        state.active_session_mut().begin_sending();
        let (mut terminal, area) = setup_term(30, 1);

        // When rendering the element.
        terminal
            .draw(|frame| {
                let slices = jinn_slices::Slices::new();
                let overlay_views = jinn_slices::OverlayViews::new();
                let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
                element.render(frame, area, &ctx);
            })
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let row = buffer_row(&buffer, 0, 30);

        // Then the label shows "Working...".
        assert!(
            row.contains("Working..."),
            "expected Working..., got: {row}"
        );
    }

    /// The theme color the session list uses for attendants.
    ///
    /// Read through the state's own theme field rather than through the theme
    /// crate, which this slice does not depend on: a color is a
    /// [`ratatui::style::Color`], and naming the type buys nothing here.
    fn attendant_fg() -> Color {
        jinn_kernel::AppState::default_with_scope_focus()
            .frontend
            .theme
            .attendant_fg
    }

    /// The theme color the session list uses for subagents.
    fn subagent_fg() -> Color {
        jinn_kernel::AppState::default_with_scope_focus()
            .frontend
            .theme
            .subagent_fg
    }

    /// An `AppState` whose active session has `origin`, idle.
    fn state_with_origin(origin: SessionOrigin) -> jinn_kernel::AppState {
        let mut state = jinn_kernel::AppState::default_with_scope_focus();
        state.active_session_mut().set_origin(origin);
        state
    }

    /// The indicator row rendered on its own, as the string it shows.
    fn render_row(state: &jinn_kernel::AppState, width: u16) -> ratatui::buffer::Buffer {
        use jinn_kernel::common::ui_element::UiElement;
        use jinn_testutil::setup_term;

        let mut element = StreamingIndicatorElement::new();
        let (mut terminal, area) = setup_term(width, 1);
        terminal
            .draw(|frame| {
                let slices = jinn_slices::Slices::new();
                let overlay_views = jinn_slices::OverlayViews::new();
                let ctx = RenderCtx::new_with_default_config(state, &slices, &overlay_views);
                element.render(frame, area, &ctx);
            })
            .expect("draw the indicator row");
        terminal.backend().buffer().clone()
    }

    /// The text of the indicator row.
    fn row_text(state: &jinn_kernel::AppState, width: u16) -> String {
        let buffer = render_row(state, width);
        (0..width)
            .filter_map(|x| buffer.cell((x, 0)).map(ratatui::buffer::Cell::symbol))
            .collect()
    }

    /// The foreground color of the cell at `x` in the indicator row.
    fn color_at(state: &jinn_kernel::AppState, width: u16, x: u16) -> Color {
        render_row(state, width)
            .cell((x, 0))
            .expect("the row is `width` wide")
            .fg
    }

    #[rstest::rstest]
    fn idle_attendant_session_names_its_kind_at_the_right_edge() {
        // Given an idle attendant session.
        let state = state_with_origin(SessionOrigin::Attendant);

        // When the indicator row is rendered.
        let text = row_text(&state, 30);

        // Then the row says so, flush against the right edge.
        assert_eq!(text, format!("{}[attendant]", " ".repeat(19)));
    }

    #[rstest::rstest]
    fn idle_attendant_session_names_its_kind_in_the_attendant_color() {
        // Given an idle attendant session.
        let state = state_with_origin(SessionOrigin::Attendant);

        // When the indicator row is rendered.
        let color = color_at(&state, 30, 20);

        // Then the label carries the color the session list uses for one.
        assert_eq!(color, attendant_fg());
    }

    #[rstest::rstest]
    fn idle_subagent_session_names_its_kind_at_the_right_edge() {
        // Given an idle subagent session.
        let state = state_with_origin(SessionOrigin::Subagent);

        // When the indicator row is rendered.
        let text = row_text(&state, 30);

        // Then the row says so, flush against the right edge.
        assert_eq!(text, format!("{}[subagent]", " ".repeat(20)));
    }

    #[rstest::rstest]
    fn idle_subagent_session_names_its_kind_in_the_subagent_color() {
        // Given an idle subagent session.
        let state = state_with_origin(SessionOrigin::Subagent);

        // When the indicator row is rendered.
        let color = color_at(&state, 30, 20);

        // Then the label carries the color the session list uses for one.
        assert_eq!(color, subagent_fg());
    }

    #[rstest::rstest]
    fn idle_user_session_names_no_kind() {
        // Given an idle session the user started themselves.
        let state = state_with_origin(SessionOrigin::User);

        // When the indicator row is rendered.
        let text = row_text(&state, 30);

        // Then nothing is drawn — an ordinary conversation needs no label.
        assert!(text.trim().is_empty(), "expected an empty row, got: {text}");
    }

    #[rstest::rstest]
    fn idle_fork_of_an_attendant_session_names_no_kind() {
        // Given an idle session forked from an attendant's conversation.
        let state = state_with_origin(SessionOrigin::Fork);

        // When the indicator row is rendered.
        let text = row_text(&state, 30);

        // Then nothing is drawn. The fork is a conversation the user is now
        // having themselves, whatever it was branched from.
        assert!(text.trim().is_empty(), "expected an empty row, got: {text}");
    }

    #[rstest::rstest]
    fn busy_attendant_session_shows_the_kind_label() {
        // Given a busy attendant session.
        let mut state = state_with_origin(SessionOrigin::Attendant);

        // When the indicator row is rendered.
        let text = row_text(&state, 40);

        // Then the label still reaches the right edge — being busy does not
        // crowd it out.
        assert!(
            text.ends_with("[attendant]"),
            "the label must reach the right edge, got: {text}"
        );
    }

    #[rstest::rstest]
    fn busy_attendant_session_keeps_the_spinner_text_at_the_left_edge() {
        // Given a busy attendant session — mid-turn in the phase sense.
        let mut state = state_with_origin(SessionOrigin::Attendant);
        state.active_session_mut().begin_streaming();

        // When the indicator row is rendered.
        let text = row_text(&state, 40);

        // Then the busy text is still where the spinner has always put it, at
        // the row's left edge rather than pushed out by the label. The throbber
        // writes its glyph and a trailing space, then the label's own leading
        // space: the text therefore starts at column two.
        assert!(
            text.contains(" Streaming...") && text.find(" Streaming...") == Some(2),
            "the busy text must sit right after the spinner glyph, got: {text}"
        );
    }

    #[rstest::rstest]
    fn busy_attendant_session_still_animates_the_spinner() {
        // Given a busy attendant session, with the indicator's own animation
        // state carrying across frames — a spinner rebuilt from a fresh state
        // each frame would sit at index 0 forever and pass a text-only assertion.
        use jinn_kernel::common::ui_element::UiElement;
        use jinn_testutil::setup_term;

        let mut state = state_with_origin(SessionOrigin::Attendant);
        state.active_session_mut().begin_streaming();
        let mut element = StreamingIndicatorElement::new();
        let (mut terminal, area) = setup_term(40, 1);

        // When the indicator row is rendered twice, an animation interval apart.
        let glyphs = std::iter::repeat_with(|| {
            std::thread::sleep(jinn_slices::SPINNER_INTERVAL + std::time::Duration::from_millis(5));
            terminal
                .draw(|frame| {
                    let slices = jinn_slices::Slices::new();
                    let overlay_views = jinn_slices::OverlayViews::new();
                    let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
                    element.render(frame, area, &ctx);
                })
                .expect("draw the indicator row");
            let buffer = terminal.backend().buffer().clone();
            buffer.cell((0, 0)).expect("a cell").symbol().to_owned()
        })
        .take(2)
        .collect::<Vec<_>>();

        // Then the glyph steps between the two frames — the label sharing the
        // row did not cost the spinner its animation state.
        assert_ne!(
            glyphs[0], glyphs[1],
            "the spinner must keep animating alongside the kind label"
        );
    }
}

#[cfg(test)]
mod registration_tests {
    use super::StreamingIndicatorElement;
    use jinn_kernel::common::AppUiRegistry;
    use jinn_kernel::common::ui_element::UiElement;

    #[rstest::rstest]
    fn register_adds_streaming_indicator() {
        // Given an empty registry.
        let mut registry = AppUiRegistry::new();

        // When registering the inference slice's UI elements.
        crate::register(&mut registry);

        // Then exactly 1 element was added (the streaming indicator).
        assert_eq!(
            registry.iter_mut().count(),
            1,
            "inference::register should add the streaming indicator"
        );
    }

    #[rstest::rstest]
    fn element_is_constructible() {
        // Given nothing.
        // When constructing the element.
        let element = StreamingIndicatorElement::new();

        // Then it is registered under its lookup name.
        assert_eq!(element.name(), "streaming-indicator");
    }
}
