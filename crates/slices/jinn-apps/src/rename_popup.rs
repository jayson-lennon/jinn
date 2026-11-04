//! The rename popup — the input-capturing scope behind `<c-r>`.
//!
//! The session-rename shape: a centered overlay rendering the
//! in-progress text from the picker cell, an input hook that serves the
//! editing intents while the popup's scope is focused, and three rows
//! (`ENTER` confirm, `<esc>` cancel, `<c-c>` clear-or-leave). The
//! confirm publishes [`RenameApp`](jinn_apps_msg::RenameApp); the store
//! actor writes cell and file, the republisher repaints the still-open
//! picker underneath.
//!
//! The popup's state lives on the picker cell ([`AppsPickerState`]'s
//! `rename_input`) rather than a second cell: the popup is the picker's
//! own affordance, and one cell per surface keeps the render path
//! single-registry.

use jinn_apps_msg::AppsPickerState;
use jinn_slices::RenderFacts;
use jinn_slices::SliceScopeId;
use jinn_slices::cell::TypedCell;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

/// The rename popup's dynamic scope (input-capturing), pushed on
/// `<c-r>` from the picker.
#[must_use]
pub fn rename_scope() -> SliceScopeId {
    jinn_apps_msg::apps_rename_scope()
}

/// Horizontal padding fraction for the rename popup (20% each side) —
/// the session-rename geometry.
const POPUP_H_PAD_FRAC: f32 = 0.20;
/// Minimum popup width in cells.
const POPUP_MIN_WIDTH: u16 = 30;

/// Computes the centered rename popup rectangle — the session-rename
/// geometry.
#[must_use]
pub fn rename_popup_rect(area: Rect) -> Rect {
    let popup_width = ((f32::from(area.width) * (1.0 - 2.0 * POPUP_H_PAD_FRAC)).ceil() as u16)
        .max(POPUP_MIN_WIDTH)
        .min(area.width);
    let popup_height = 3u16.min(area.height);

    #[expect(clippy::integer_division, reason = "cell positions are integers")]
    let popup_x = area.width.saturating_sub(popup_width) / 2;
    #[expect(clippy::integer_division, reason = "cell positions are integers")]
    let popup_y = area.height.saturating_sub(popup_height) / 3;

    Rect::new(popup_x, popup_y, popup_width, popup_height)
}

/// The overlay-rect function registered on the slice host.
#[must_use]
pub fn rename_overlay_rect(area: &Rect) -> Option<Rect> {
    Some(rename_popup_rect(*area))
}

/// The overlay view: draws the rename popup into `area`, reading the
/// edit state from the picker cell.
pub fn render_rename_overlay(frame: &mut Frame<'_>, area: Rect, ctx: &RenderFacts) {
    let Some(cell) = ctx
        .slices
        .reader::<AppsPickerState>(&jinn_apps_msg::apps_picker_slot())
    else {
        return;
    };
    let state = cell.read();
    draw(
        frame,
        area,
        &state.rename_input.input,
        state.rename_input.cursor_pos,
        &ctx.theme,
    );
}

/// Draws the popup chrome + input line into `popup_area` — the
/// session-rename draw, re-titled.
fn draw(
    frame: &mut Frame<'_>,
    popup_area: Rect,
    input_text: &str,
    input_cursor: usize,
    theme: &jinn_theme::Theme,
) {
    let title = Line::from(Span::styled(
        " Rename App ",
        Style::default().fg(theme.popup_title),
    ));

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border_unfocused));

    frame.render_widget(Clear, popup_area);
    frame.render_widget(block, popup_area);

    let inner = Rect {
        x: popup_area.x + 1,
        y: popup_area.y + 1,
        width: popup_area.width.saturating_sub(2),
        height: popup_area.height.saturating_sub(2),
    };

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let prefix = Span::styled("> ", Style::default().fg(theme.focus_accent));
    let input_span = Span::raw(input_text);
    let input_line = Line::from(vec![prefix, input_span]);
    frame.render_widget(
        Paragraph::new(input_line),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );

    let prefix_len = 2u16;
    let grapheme_count = {
        // Count display columns the way the shared LineInput does —
        // grapheme-aware, never byte-indexed.
        use unicode_segmentation::UnicodeSegmentation as _;
        input_text
            .get(..input_cursor)
            .map_or(0, |s| s.graphemes(true).count())
    };
    let cursor_x = (prefix_len + grapheme_count as u16).min(inner.width.saturating_sub(1));
    frame.set_cursor_position((inner.x.saturating_add(cursor_x), inner.y));
}

/// The rename popup's route rows: confirm, cancel, clear-or-leave.
pub fn attach_rename_rows(routes: &jinn_slices::KeyRoutes, picker: &TypedCell<AppsPickerState>) {
    use jinn_slices::RouteId;
    use jinn_slices::RouteResult as IntentResult;
    use jinn_slices::route::{ActionFn, BindSite, RouteOutcome, RouteRow};

    let confirm_picker = picker.clone();
    routes.attach(RouteRow {
        route_id: RouteId::new("apps-rename-confirm"),
        scope: rename_scope(),
        key: "<enter>",
        category: "input",
        site: BindSite::OwnScope,
        feature: "apps",
        outcome: RouteOutcome::Action {
            action: "apps-rename-confirm",
            display: "rename the app",
            run: ActionFn::new(move |_ctx| crate::picker_actions::confirm_rename(&confirm_picker)),
        },
    });

    let cancel_picker = picker.clone();
    routes.attach(RouteRow {
        route_id: RouteId::new("apps-rename-cancel"),
        scope: rename_scope(),
        key: "<esc>",
        category: "general",
        site: BindSite::OwnScope,
        feature: "apps",
        outcome: RouteOutcome::Action {
            action: "apps-rename-cancel",
            display: "cancel rename",
            run: ActionFn::new(move |_ctx| crate::picker_actions::cancel_rename(&cancel_picker)),
        },
    });

    let clear_picker = picker.clone();
    routes.attach(RouteRow {
        route_id: RouteId::new("apps-rename-clear-or-leave"),
        scope: rename_scope(),
        key: "<c-c>",
        category: "input",
        site: BindSite::OwnScope,
        feature: "apps",
        outcome: RouteOutcome::Action {
            action: "apps-rename-clear-or-leave",
            display: "clear the title, or leave when already empty",
            run: ActionFn::new(move |_ctx| {
                crate::picker_actions::clear_or_leave_rename(&clear_picker)
            }),
        },
    });

    // The popup's input hook: the editing intents write the popup's
    // LineInput while its scope is focused.
    let hook_picker = picker.clone();
    let hook: jinn_slices::route::InputHook =
        std::sync::Arc::new(move |intent: &jinn_slices::route::EditIntent| {
            hook_picker.update(|state| match intent {
                jinn_slices::route::EditIntent::InsertChar(ch) => {
                    state.rename_input.insert_char(*ch);
                }
                jinn_slices::route::EditIntent::DeleteBackward => state.rename_input.delete(),
                jinn_slices::route::EditIntent::DeleteForward => {
                    state.rename_input.delete_forward();
                }
                jinn_slices::route::EditIntent::CursorLeft => state.rename_input.cursor_left(),
                jinn_slices::route::EditIntent::CursorRight => state.rename_input.cursor_right(),
                jinn_slices::route::EditIntent::CursorHome => state.rename_input.cursor_home(),
                jinn_slices::route::EditIntent::CursorEnd => state.rename_input.cursor_end(),
                jinn_slices::route::EditIntent::Paste(text) => state.rename_input.paste(text),
            });
            Some(IntentResult::empty())
        });
    routes.register_input_hook(&rename_scope(), hook);
}
