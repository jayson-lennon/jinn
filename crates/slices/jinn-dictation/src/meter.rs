//! The dictation meter strip — the minimap column from the streaming
//! indicator row to the terminal bottom edge.
//!
//! Idle it paints a dotted placeholder in the inactive border color
//! with an orange `i` glyph at the bottom cell (the alt-shortcut
//! hint). While arming — the press asked for the microphone but the
//! device open has not decided yet — the dots turn the theme's
//! warning yellow, the honest "working on it" signal. Recording it
//! paints the theme's success green with a level-proportional fill
//! growing from the bottom, capped by a partial block at the level
//! boundary; the green appears only once the capture stream actually
//! plays. All colors come from existing theme keys.
//!
//! Geometry is composition's, not ours: the dispatch hands the paint
//! the strip rect ([`jinn_slices::DrawTarget::area`]), computed by
//! `AppLayout::new` as the minimap column below the indicator row, and
//! that row's anchor rides the input box, so the strip resizes as the
//! input grows. The painter carries no layout arithmetic of its own —
//! a layout change that moves the strip moves it here too,
//! automatically. The region dispatches on chat layouts only; the
//! full-width tab path never reaches it, and that rule is structural:
//! `TabLayout` has no `meter_strip` field, so the tab arm physically
//! cannot hand the dispatcher a strip.
//!
//! Registration follows the status-bar precedent: an always-painted
//! draw function on a region, not a scope-gated overlay (an overlay
//! would vanish the instant focus left any dictation scope, and
//! recording outlives scope changes by design).

use jinn_dictation_msg::DictationLevel;
use jinn_dictation_msg::DictationPhase;
use jinn_dictation_msg::DictationState;
use jinn_dictation_msg::dictation_level_slot;
use jinn_dictation_msg::dictation_state_slot;
use jinn_kernel::common::app_state::AppState;
use jinn_slices::DrawContext;
use ratatui::layout::Rect;

/// The partial-fill cap glyphs, shortest block first. `█` completes the
/// scale and is reached as the full-fill case, not as a cap.
const CAP_GLYPHS: [&str; 7] = ["▁", "▂", "▃", "▄", "▅", "▆", "▇"];

/// Paints the meter: the region draw function the slice registers.
///
/// Reads the state cell (the dictation actor is the sole writer) and
/// the level cell, then paints the strip for the phase: dotted idle
/// cells, or the recording bar. `target.area` is the strip rect,
/// composition-owned — this function derives no geometry.
pub fn paint(
    frame: &mut ratatui::Frame<'_>,
    target: jinn_slices::DrawTarget,
    ctx: &dyn DrawContext<AppState>,
) {
    let rect = target.area;
    let state = ctx.state();
    let phase = ctx
        .slices()
        .reader::<DictationState>(&dictation_state_slot())
        .map_or(DictationPhase::Offline, |cell| cell.read().phase);
    let theme = &state.frontend.theme;
    match phase {
        DictationPhase::Recording => {
            let level = ctx
                .slices()
                .reader::<DictationLevel>(&dictation_level_slot())
                .map_or(0.0, |cell| cell.read().level);
            paint_recording(frame, rect, level, state);
        }
        // Arming: the press asked, the device open is running. The
        // dotted strip stays (nothing is recording yet) but the dots
        // say "working on it" in the warning yellow.
        DictationPhase::Arming => paint_idle(frame, rect, theme.warning, state),
        _ => paint_idle(frame, rect, theme.border_unfocused, state),
    }
}

/// Paints the idle strip into `rect`: one dotted cell per row in
/// `dot_color`, with the hint glyph at the bottom. The background
/// resets with the dots — the strip covers cells nothing else
/// repaints, so a stale recording green would otherwise linger. The
/// dot color is the phase's: inactive-border idle, warning yellow
/// while arming.
///
/// Split from [`paint`] so the rendering is testable without cells:
/// the test hands a rect, a dot color, and a state, and asserts the
/// buffer.
pub fn paint_idle(
    frame: &mut ratatui::Frame<'_>,
    rect: Rect,
    dot_color: ratatui::style::Color,
    state: &AppState,
) {
    if rect.height == 0 || rect.width == 0 {
        return;
    }
    for y in rect.y..(rect.y + rect.height) {
        let Some(cell) = frame.buffer_mut().cell_mut((rect.x, y)) else {
            continue;
        };
        cell.set_symbol("┇");
        cell.set_fg(dot_color);
        cell.set_bg(ratatui::style::Color::Reset);
    }
    paint_hint_glyph(frame, rect, state);
}

/// Paints the recording strip into `rect`.
///
/// Split from [`paint`] so the level-driven rendering is testable
/// without cells: the test hands a rect, a level, and a state, and
/// asserts the buffer.
pub fn paint_recording(frame: &mut ratatui::Frame<'_>, rect: Rect, level: f32, state: &AppState) {
    paint_recording_background(frame, rect, state);
    paint_level_fill(frame, rect, level);
    paint_hint_glyph(frame, rect, state);
}

/// Paints the recording background: the full strip in the theme's
/// success green.
fn paint_recording_background(frame: &mut ratatui::Frame<'_>, rect: Rect, state: &AppState) {
    let background = state.frontend.theme.success;
    let text_fg = state.frontend.theme.primary_text;
    for y in rect.y..(rect.y + rect.height) {
        let Some(cell) = frame.buffer_mut().cell_mut((rect.x, y)) else {
            continue;
        };
        cell.set_symbol(" ");
        cell.set_bg(background);
        cell.set_fg(text_fg);
    }
}

/// Paints the level fill above the hint glyph: full blocks growing
/// from the bottom meter cell, capped by one partial block at the
/// level boundary. A zero level paints nothing — the bare green strip
/// is the zero reading.
fn paint_level_fill(frame: &mut ratatui::Frame<'_>, rect: Rect, level: f32) {
    if level <= 0.0 {
        return;
    }
    let meter_cells = rect.height.saturating_sub(1);
    if meter_cells == 0 {
        return;
    }
    let hint_row = rect.y + rect.height.saturating_sub(1);
    // `filled` never exceeds `meter_cells`, and the hint row is at least
    // `meter_cells` above the strip top, so every offset below stays in
    // the rect.
    let scaled = level.clamp(0.0, 1.0) * f32::from(meter_cells);
    let filled = (scaled.floor() as u16).min(meter_cells);
    for y in (hint_row - filled)..hint_row {
        let Some(cell) = frame.buffer_mut().cell_mut((rect.x, y)) else {
            continue;
        };
        cell.set_symbol("█");
    }
    if filled < meter_cells {
        let remainder = scaled - f32::from(filled);
        let cap_index = ((remainder * 8.0).floor() as usize).min(CAP_GLYPHS.len() - 1);
        let cap_y = hint_row - (filled + 1);
        if let (Some(cell), Some(cap)) = (
            frame.buffer_mut().cell_mut((rect.x, cap_y)),
            CAP_GLYPHS.get(cap_index),
        ) {
            cell.set_symbol(cap);
        }
    }
}

/// Paints the alt-hint glyph: the `i` at the strip's bottom cell in
/// the theme's hotkey accent, identical in every phase so the
/// shortcut never moves.
fn paint_hint_glyph(frame: &mut ratatui::Frame<'_>, rect: Rect, state: &AppState) {
    if rect.height == 0 || rect.width == 0 {
        return;
    }
    let hint_fg = state.frontend.theme.accent_action;
    let Some(cell) = frame
        .buffer_mut()
        .cell_mut((rect.x, rect.y + rect.height - 1))
    else {
        return;
    };
    cell.set_symbol("i");
    cell.set_fg(hint_fg);
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic,
        clippy::semicolon_if_nothing_returned,
        reason = "test code, panics are acceptable"
    )]
    use super::paint_idle;
    use super::paint_recording;
    use jinn_app_state::AppState;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;

    const FRAME: Rect = Rect::new(0, 0, 80, 24);

    /// A state wired like production: catalog cells attached.
    fn wired_state() -> AppState {
        AppState::default_with_scope_focus()
    }

    #[rstest::rstest]
    #[test]
    fn idle_paints_dotted_cells_and_the_constant_hint() {
        // Given a wired state and an idle strip rect.
        let state = wired_state();
        let rect = Rect::new(20, 2, 1, 8);
        let backend = TestBackend::new(FRAME.width, FRAME.height);
        let mut terminal = Terminal::new(backend).expect("test backend");
        let theme = state.frontend.theme.clone();

        // When painting the idle strip.
        terminal
            .draw(|frame| paint_idle(frame, rect, theme.border_unfocused, &state))
            .expect("draw into the test backend");

        // Then every cell above the bottom is a dotted glyph in the
        // inactive border color on a reset background.
        let buffer = terminal.backend().buffer();
        for y in rect.y..(rect.y + rect.height - 1) {
            let cell = buffer[(rect.x, y)].clone();
            assert_eq!(cell.symbol(), "┇");
            assert_eq!(cell.fg, theme.border_unfocused);
            assert_eq!(cell.bg, ratatui::style::Color::Reset);
        }
        // And the bottom cell is the hotkey-orange `i`, in idle just
        // as in recording.
        let hint = buffer[(rect.x, rect.y + rect.height - 1)].clone();
        assert_eq!(hint.symbol(), "i");
        assert_eq!(hint.fg, theme.accent_action);
    }

    #[rstest::rstest]
    #[test]
    fn arming_paints_yellow_dots_with_the_accent_hint() {
        // Given a wired state and a strip rect, mid-arming.
        let state = wired_state();
        let rect = Rect::new(20, 2, 1, 8);
        let backend = TestBackend::new(FRAME.width, FRAME.height);
        let mut terminal = Terminal::new(backend).expect("test backend");
        let theme = state.frontend.theme.clone();

        // When painting the arming strip: the dotted idle shape in the
        // warning yellow.
        terminal
            .draw(|frame| paint_idle(frame, rect, theme.warning, &state))
            .expect("draw into the test backend");

        // Then every cell above the bottom is a dotted glyph in the
        // warning yellow on a reset background — the "working on it"
        // signal, still not a recording bar.
        let buffer = terminal.backend().buffer();
        for y in rect.y..(rect.y + rect.height - 1) {
            let cell = buffer[(rect.x, y)].clone();
            assert_eq!(cell.symbol(), "┇");
            assert_eq!(cell.fg, theme.warning);
            assert_eq!(cell.bg, ratatui::style::Color::Reset);
        }
        // And the bottom cell is the hotkey-orange `i`.
        let hint = buffer[(rect.x, rect.y + rect.height - 1)].clone();
        assert_eq!(hint.symbol(), "i");
        assert_eq!(hint.fg, theme.accent_action);
    }

    #[rstest::rstest]
    #[test]
    fn recording_paints_green_bar_with_a_partial_cap() {
        // Given a wired state and a 10-row strip (9 meter cells + hint).
        let state = wired_state();
        let rect = Rect::new(20, 2, 1, 10);
        let backend = TestBackend::new(FRAME.width, FRAME.height);
        let mut terminal = Terminal::new(backend).expect("test backend");
        let theme = state.frontend.theme.clone();

        // When painting a half-level recording strip.
        terminal
            .draw(|frame| paint_recording(frame, rect, 0.5, &state))
            .expect("draw into the test backend");

        // Then the strip is theme-success: the bottom meter cell holds a
        // full block on green.
        let buffer = terminal.backend().buffer();
        let bottom_meter = buffer[(rect.x, rect.y + rect.height - 2)].clone();
        assert_eq!(bottom_meter.symbol(), "█");
        assert_eq!(bottom_meter.bg, theme.success);
        // And the cell one above the filled run is the partial cap —
        // 4 full cells of 9 leaves a 0.5 remainder, the `▅` glyph.
        let cap = buffer[(rect.x, rect.y + rect.height - 6)].clone();
        assert_eq!(cap.symbol(), "▅");
        // And the cells above the cap are empty green strip.
        let top = buffer[(rect.x, rect.y)].clone();
        assert_eq!(top.symbol(), " ");
        assert_eq!(top.bg, theme.success);
        // And the hint glyph is untouched hotkey-orange `i`.
        let hint = buffer[(rect.x, rect.y + rect.height - 1)].clone();
        assert_eq!(hint.symbol(), "i");
        assert_eq!(hint.fg, theme.accent_action);
    }

    #[rstest::rstest]
    #[test]
    fn full_scale_level_fills_every_meter_cell() {
        // Given a wired state and a 6-row strip (5 meter cells + hint).
        let state = wired_state();
        let rect = Rect::new(20, 2, 1, 6);
        let backend = TestBackend::new(FRAME.width, FRAME.height);
        let mut terminal = Terminal::new(backend).expect("test backend");

        // When painting at full scale.
        terminal
            .draw(|frame| paint_recording(frame, rect, 1.0, &state))
            .expect("draw into the test backend");

        // Then every meter cell is a filled glyph — the bar never covers
        // the hint.
        let buffer = terminal.backend().buffer();
        for y in rect.y..(rect.y + rect.height - 1) {
            assert_eq!(buffer[(rect.x, y)].symbol(), "█");
        }
        // And the hint glyph sits below the fill.
        assert_eq!(buffer[(rect.x, rect.y + rect.height - 1)].symbol(), "i");
    }

    #[rstest::rstest]
    #[test]
    fn zero_level_paints_a_bare_green_strip() {
        // Given a wired state and a recording strip rect.
        let state = wired_state();
        let rect = Rect::new(20, 2, 1, 8);
        let backend = TestBackend::new(FRAME.width, FRAME.height);
        let mut terminal = Terminal::new(backend).expect("test backend");
        let theme = state.frontend.theme.clone();

        // When painting with a zero level.
        terminal
            .draw(|frame| paint_recording(frame, rect, 0.0, &state))
            .expect("draw into the test backend");

        // Then the strip is green with no glyphs at all — zero shows
        // nothing but the background.
        let buffer = terminal.backend().buffer();
        for y in rect.y..(rect.y + rect.height - 1) {
            let cell = buffer[(rect.x, y)].clone();
            assert_eq!(cell.symbol(), " ");
            assert_eq!(cell.bg, theme.success);
        }
        // And the hint glyph is the only glyph on the strip.
        let hint = buffer[(rect.x, rect.y + rect.height - 1)].clone();
        assert_eq!(hint.symbol(), "i");
        assert_eq!(hint.fg, theme.accent_action);
    }
}
