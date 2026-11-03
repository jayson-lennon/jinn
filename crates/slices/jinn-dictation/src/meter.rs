//! The recording meter — the seam between the main column and the
//! sidebar.
//!
//! A vertical strip riding the border column's chat-log stretch: idle it
//! paints nothing (the region dispatches, the paint no-ops), recording
//! it paints the theme's success green with level-proportional bars in
//! the regular text foreground and an orange `i` glyph at the bottom —
//! the alt-shortcut hint. All colors come from existing theme keys.
//!
//! Registration follows the status-bar precedent: an always-painted
//! draw function on a region, not a scope-gated overlay (an overlay
//! would vanish the instant focus left any dictation scope, and
//! recording outlives scope changes by design). Composition hands the
//! draw function the full frame area; the geometry is *derived* here
//! from the same state inputs the layout computed
//! (`frontend.sidebar_width`, the active input's visual line count) —
//! the render test pins the derived rect to the layout's border column
//! so the two derivations cannot drift silently.
//!
//! The region dispatches on chat layouts only; the full-width tab path
//! never dispatches it, which is the whole hidden-on-tabs rule.
//!
//! # The derivation, spelled out
//!
//! `AppLayout::new` (composition) computes, in order: the sidebar width
//! clamped to `[MIN_SIDEBAR_WIDTH, area.width - MIN_WIDTH]`; the main
//! column as the remainder minus the 1-column minimap and 1-column
//! border; the border at `main.x + main.width + 1`; the vertical split
//! `tab(1) / content(min 1) / input(1 + lines) / status(2)`. The meter
//! repeats exactly that arithmetic and takes the border column's
//! chat-log stretch (content minus the bottom 2 rows). A layout change
//! that breaks the meter shows up as the geometry test failing — never
//! as a silently misplaced strip.

use jinn_dictation_msg::DictationLevel;
use jinn_dictation_msg::DictationPhase;
use jinn_dictation_msg::DictationState;
use jinn_dictation_msg::dictation_level_slot;
use jinn_dictation_msg::dictation_state_slot;
use jinn_kernel::common::app_state::AppState;
use jinn_slices::DrawContext;
use ratatui::layout::Rect;

/// The meter's width: one column, riding the border column.
pub const METER_WIDTH: u16 = 1;

/// The chat-content bottom rows the meter does not span (the streaming
/// indicator row and the chat bottom line), the same split
/// `chat_tab` renders with.
const BOTTOM_LINES: u16 = 2;

/// Minimum sidebar width, mirrored from composition's `AppLayout`
/// (`MIN_SIDEBAR_WIDTH`). The layout clamps the configured width up to
/// this; the derivation must match or the strip drifts off the seam.
pub const MIN_SIDEBAR_WIDTH: u16 = 15;

/// Minimum terminal width, mirrored from composition's `AppLayout`
/// (`MIN_WIDTH`).
pub const MIN_WIDTH: u16 = 40;

/// The derived meter rect for one frame.
///
/// `full_width_tab` is the base-scope test the render pass runs
/// (`true` when the base scope is a registered tab scope) — `None`
/// there, because full-width tabs have no seam to ride. The arithmetic
/// repeats composition's `AppLayout::new` from the same inputs;
/// `meter_rect_matches_the_layout_seam` pins the two together.
#[must_use]
pub fn meter_rect(state: &AppState, frame: Rect, full_width_tab: bool) -> Option<Rect> {
    if full_width_tab {
        return None;
    }
    // Horizontal split (composition's AppLayout::new, lines copied in
    // the same order): sidebar clamp, main width, border column.
    let sidebar_width = state
        .frontend
        .sidebar_width
        .min(frame.width.saturating_sub(MIN_WIDTH))
        .max(MIN_SIDEBAR_WIDTH);
    let border_width = 1_u16;
    let minimap_width = 1_u16;
    let main_width = frame
        .width
        .saturating_sub(sidebar_width)
        .saturating_sub(border_width)
        .saturating_sub(minimap_width);
    let border_x = frame.x + main_width + minimap_width;

    // Vertical split (same function): 1 tab bar, min-1 content,
    // 1 + visual-lines input, 2-row status bar.
    let input_lines = state.active_session().with_input(
        jinn_chat_input_msg::ChatInputBoxState::visual_line_count,
        || 0,
    ) as u16;
    let input_height = (1 + input_lines.max(1)).min(frame.height / 2);
    let [_, content, _, _] = ratatui::layout::Layout::vertical([
        ratatui::layout::Constraint::Length(1),
        ratatui::layout::Constraint::Min(1),
        ratatui::layout::Constraint::Length(input_height),
        ratatui::layout::Constraint::Length(2),
    ])
    .areas(frame);

    Some(Rect {
        x: border_x,
        y: content.y,
        width: METER_WIDTH,
        height: content.height.saturating_sub(BOTTOM_LINES),
    })
}

/// Paints the meter: the region draw function the slice registers.
///
/// Reads the state cell (the dictation actor is the sole writer) and the
/// level cell; `Recording` paints the strip, every other phase paints
/// nothing — idle invisibility against the border is a no-op, not a
/// background wash.
pub fn paint(
    frame: &mut ratatui::Frame<'_>,
    target: jinn_slices::DrawTarget,
    ctx: &dyn DrawContext<AppState>,
) {
    let state = ctx.state();
    // The same base-scope test the render pass runs to pick the tab
    // layout: a base scope in the registered tab set renders full width,
    // and full width has no seam to ride.
    let full_width_tab = match state.frontend.scope_base() {
        jinn_slices::FocusScope::Dynamic(id) => ctx.slices().tab_scopes().contains(&id),
        _ => false,
    };
    let Some(rect) = meter_rect(state, target.area, full_width_tab) else {
        return;
    };
    let phase = ctx
        .slices()
        .reader::<DictationState>(&dictation_state_slot())
        .map(|cell| cell.read().phase)
        .unwrap_or(DictationPhase::Offline);
    if phase != DictationPhase::Recording {
        return;
    }
    let level = ctx
        .slices()
        .reader::<DictationLevel>(&dictation_level_slot())
        .map(|cell| cell.read().level)
        .unwrap_or(0.0);
    paint_recording(frame, rect, level, ctx.state());
}

/// Paints the recording strip into `rect`.
///
/// Split from [`paint`] so the level-driven rendering is testable
/// without cells: the test hands a rect, a level, and a state, and
/// asserts the buffer.
pub fn paint_recording(frame: &mut ratatui::Frame<'_>, rect: Rect, level: f32, state: &AppState) {
    let theme = &state.frontend.theme;
    let background = theme.success;
    let bars_fg = theme.primary_text;
    let hint_fg = theme.warning;

    // The strip: solid theme-success background, blank cells.
    for y in rect.y..(rect.y + rect.height) {
        let Some(cell) = frame.buffer_mut().cell_mut((rect.x, y)) else {
            continue;
        };
        cell.set_symbol(" ");
        cell.set_bg(background);
        cell.set_fg(bars_fg);
    }
    // The bar: level-proportional, growing from the bottom, block glyphs
    // over the strip. The glyph rows never cover the hint row at the
    // bottom, so a full-scale level clamps above it.
    let bar_space = rect.height.saturating_sub(1);
    let filled = ((level * f32::from(bar_space)).round() as u16).min(bar_space);
    for y in (rect.y + rect.height - filled)..(rect.y + rect.height) {
        let Some(cell) = frame.buffer_mut().cell_mut((rect.x, y)) else {
            continue;
        };
        cell.set_symbol("█");
    }
    // The alt-hint glyph at the strip's bottom: orange `i`.
    if rect.height > 0
        && let Some(cell) = frame
            .buffer_mut()
            .cell_mut((rect.x, rect.y + rect.height - 1))
    {
        cell.set_symbol("i");
        cell.set_fg(hint_fg);
    }
}
