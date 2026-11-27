//! Status bar - displays the session CWD, active prompt strategy, and current model.
//!
//! Shows the session's working directory on line 1, and status information
//! on line 2: token stats, turn count, and model. The model shows
//! `({provider})/{model}` when set, or "no model selected" otherwise.
//!
//! The per-session presentation is shared: [`render_session_cwd_line`] and
//! [`render_session_info_line`] draw the two lines for *any* session, not
//! just the active one, so other surfaces (the sidebar's session preview)
//! render the identical status line. The element composes them with the two
//! active-session-only extras — the tree aggregate and the transient hint —
//! which are deliberately not part of the shared surface.

use std::collections::HashMap;

use jiff::{SignedDuration, Timestamp};
use jinn_common::shorten_path;
use jinn_core_types::{SessionId, WorkingInterval};
use jinn_kernel::common::app_state::AppState;
use jinn_kernel::common::render_ctx::RenderCtx;
use jinn_kernel::common::ui_element::UiElement;
use jinn_provider_config::ModelCache;
use jinn_session_state::ChatSessionState;
use jinn_slices::DrawContext;
use jinn_status_bar_msg::StatusBarState;
use jinn_status_bar_msg::session_stats;
use jinn_status_bar_msg::status_bar_slot;
use jinn_status_bar_msg::tree_is_shown;
use jinn_status_bar_msg::tree_stats;
use jinn_theme::Theme;
use jinn_work_time_msg::WorkingTimeState;
use jinn_work_time_msg::work_time_slot;
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

/// A display element that shows the active strategy and provider/model in the status bar.
#[derive(Debug)]
pub struct StatusBarElement;

/// Paints the status bar into `area`.
///
/// Registered as this slice's draw function for
/// [`jinn_slices::Region::StatusBar`], so the composition layer asks
/// the region who draws it rather than naming this function.
pub fn paint(frame: &mut Frame<'_>, area: Rect, ctx: &dyn DrawContext<AppState>) {
    let state = ctx.state();
    {
        // Split area into cwd line + info line.
        let [cwd_area, info_area] =
            Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(area);

        let style = Style::default().fg(state.frontend.theme.muted_text);
        let model_cache = model_cache_of(state);

        // The right side is hint-OR-model, decided before anything draws: a
        // hint up means the override line, no hint means the model. One
        // either/or widget is what the original bar rendered, so a short
        // hint never leaves the tail of a longer model string beside it.
        let right_override = transient_hint(ctx.slices())
            .map(|hint| hint_override_line(&hint, &state.frontend.theme));

        render_session_cwd_line(frame, cwd_area, state.active_session(), style);
        render_tree_aggregate(frame, cwd_area, state, style, ctx.slices());
        render_session_info_line(
            frame,
            info_area,
            state.active_session(),
            &state.frontend.theme,
            model_cache.as_ref(),
            active_working_time(state, ctx.slices()),
            style,
            right_override,
        );
    }
}

impl UiElement for StatusBarElement {
    fn name(&self) -> String {
        "status-bar".to_owned()
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx) {
        let state = ctx.state;
        // Split area into cwd line + info line.
        let [cwd_area, info_area] =
            Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(area);

        let style = Style::default().fg(state.frontend.theme.muted_text);
        let model_cache = model_cache_of(state);

        let right_override = transient_hint(ctx.slices())
            .map(|hint| hint_override_line(&hint, &state.frontend.theme));

        render_session_cwd_line(frame, cwd_area, state.active_session(), style);
        render_tree_aggregate(frame, cwd_area, state, style, ctx.slices());
        render_session_info_line(
            frame,
            info_area,
            state.active_session(),
            &state.frontend.theme,
            model_cache.as_ref(),
            active_working_time(state, ctx.slices()),
            style,
            right_override,
        );
    }
}

/// Renders the CWD line (left-aligned) for `session`.
///
/// The shared half of the status line: the sidebar's session preview calls
/// this with the *previewed* session, so both surfaces draw the identical
/// line — home collapsed to `~`, left-aligned, muted.
pub fn render_session_cwd_line(
    frame: &mut Frame<'_>,
    area: Rect,
    session: &ChatSessionState,
    style: Style,
) {
    let cwd = session.cwd();
    let cwd_display = shorten_path(cwd);
    let cwd_widget = Paragraph::new(Line::from(Span::styled(cwd_display, style)))
        .style(style)
        .alignment(Alignment::Left);
    frame.render_widget(cwd_widget, area);
}

/// The working intervals of every live session, read from the work-time cell.
///
/// Resolved through the frontend's attached registry. An absent cell — the
/// slice not activated, or a test that never ran the catalog — yields no
/// intervals rather than a panic, so the bar degrades to showing no working
/// time instead of failing to draw.
fn live_working_intervals(
    slices: &jinn_slices::Slices,
) -> HashMap<SessionId, Vec<WorkingInterval>> {
    slices
        .reader::<WorkingTimeState>(&work_time_slot())
        .map_or_else(HashMap::new, |cell| cell.read().snapshot())
}

/// The active session's working time, measured live so a running turn ticks.
fn active_working_time(state: &AppState, slices: &jinn_slices::Slices) -> SignedDuration {
    session_working_time(state.session.active_session_id(), slices)
}

/// A session's working time, read live from the work-time cell.
///
/// The shared counterpart of [`active_working_time`]: keyed by the caller's
/// session id rather than the active one, so a surface describing a
/// background session (the sidebar's session preview) clocks *that*
/// session's working time. An absent cell — the slice not activated, or a
/// test that never ran the catalog — degrades to zero rather than panicking,
/// so the caller still renders a clock.
pub fn session_working_time(
    session_id: &SessionId,
    slices: &jinn_slices::Slices,
) -> SignedDuration {
    slices
        .reader::<WorkingTimeState>(&work_time_slot())
        .map_or(SignedDuration::ZERO, |cell| {
            cell.read().working_time(session_id, Timestamp::now())
        })
}

/// Renders the tree aggregate (right-aligned on the CWD line) when the tree has >1 session.
fn render_tree_aggregate(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    style: Style,
    slices: &jinn_slices::Slices,
) {
    let tree = tree_stats(
        state.session.sessions(),
        state.session.frozen_nodes(),
        state.session.active_session_id(),
        &live_working_intervals(slices),
    );
    if !tree_is_shown(&tree) {
        return;
    }
    let up_arrow = '\u{2191}';
    let down_arrow = '\u{2193}';
    let turn_symbol = '\u{21BB}';
    let session_symbol = '\u{29C9}';
    let tree_prefix = '\u{1F333}';
    let turns = tree.turn_count;
    let count = tree.session_count;
    let mut tree_spans: Vec<Span> = vec![Span::styled(format!("{tree_prefix} "), style)];
    match tree.cache_hit_percent {
        Some(pct) => {
            let cache_glyph = '\u{2B22}'; // ⬢
            tree_spans.push(Span::styled(
                format!("{cache_glyph} {pct}% "),
                cache_hit_style(&state.frontend.theme, pct),
            ));
        }
        None => {}
    }
    tree_spans.push(Span::styled(
        format!("{up_arrow}{} ", tree.tokens_sent_label()),
        style,
    ));
    tree_spans.push(Span::styled(
        format!("{down_arrow}{} ", tree.tokens_received_label()),
        style,
    ));
    tree_spans.push(Span::styled(format!(" ${:.5} ", tree.total_cost), style));
    tree_spans.push(Span::styled(format!("{turn_symbol}{turns}"), style));
    tree_spans.push(Span::styled(format!(" {}", tree.working_label()), style));
    tree_spans.push(Span::styled(format!(" {session_symbol}{count}"), style));
    let tree_widget = Paragraph::new(Line::from(tree_spans)).alignment(Alignment::Right);
    frame.render_widget(tree_widget, area);
}

/// Renders the token/model info line (left: token stats + cost + turns; right: model).
///
/// The shared half of the status line: every value is read from `session`,
/// never from any active session, so a caller describing a background
/// session shows that session's own numbers. The right side is the
/// session's model unless `right_override` supplies a line — the one seam
/// the status bar uses to keep its hint-replaces-model behavior, and the
/// reason the override is a parameter rather than something this function
/// reads: the hint is active-session chrome, so a preview of a background
/// session passes `None` and always ends in *its* model.
///
/// The override must be a *replacement*, not an overlay: the right side is
/// rendered as a single widget, so a short hint never leaves the tail of a
/// longer model string beside it. Two right-aligned widgets over one row
/// would — a `Paragraph` writes only the cells its glyphs occupy.
///
/// The two inputs that are not per-session are passed in:
/// `model_cache` (app-level, for the context limit and the modality
/// indicator — absent degrades to `???` and text-only exactly as the status
/// bar degrades with no provider cell) and `working_time` (precomputed by
/// the caller via [`session_working_time`], because the cell it reads lives
/// behind a `Slices` handle render chains don't all carry).
pub fn render_session_info_line(
    frame: &mut Frame<'_>,
    area: Rect,
    session: &ChatSessionState,
    theme: &Theme,
    model_cache: Option<&ModelCache>,
    working_time: SignedDuration,
    style: Style,
    right_override: Option<Line<'static>>,
) {
    let stats = session_stats(session, model_cache, working_time);

    let left_side = {
        let turn_symbol = '\u{21BB}';
        let mut left_spans: Vec<Span> = Vec::new();
        // Cache-hit percentage — leftmost, shown only when there are cache hits.
        if let Some(pct) = stats.cache_hit_percent {
            let cache_glyph = '\u{2B22}'; // ⬢
            left_spans.push(Span::styled(
                format!("{cache_glyph} {pct}% "),
                cache_hit_style(theme, pct),
            ));
        }
        left_spans.push(Span::styled(
            format!(
                "\u{2191}{} \u{2193}{} {}",
                stats.tokens_sent_label(),
                stats.tokens_received_label(),
                stats.context_label(),
            ),
            style,
        ));
        left_spans.push(Span::styled(format!(" {}", stats.cost_label()), style));
        left_spans.push(Span::styled(
            format!(
                " {turn_symbol}{} {}",
                stats.turn_count,
                stats.working_label()
            ),
            style,
        ));
        Paragraph::new(Line::from(left_spans))
            .style(style)
            .alignment(Alignment::Left)
    };
    frame.render_widget(left_side, area);

    // One widget for the right side: the override when the caller supplies
    // one (the status bar's transient hint), otherwise the model. Either/or
    // in a single render is what keeps a shorter hint from leaving model
    // residue beside it.
    let right_line = right_override.unwrap_or_else(|| Line::from(Span::styled(stats.model, style)));
    let right_side = Paragraph::new(right_line).alignment(Alignment::Right);
    frame.render_widget(right_side, area);
}

/// The transient hint (e.g. an inert overlay toggle) from the slice's cell.
///
/// Read at render time; an absent cell — the slice not activated, or a test
/// that never ran the catalog — means no hint, so the model shows. The hint
/// itself is *not* rendered here: the status bar composes it as the shared
/// info line's right-side override, so hint and model are one either/or
/// widget and a short hint cannot leave the tail of a longer model string
/// beside it.
fn transient_hint(slices: &jinn_slices::Slices) -> Option<String> {
    let cell = slices.reader::<StatusBarState>(&status_bar_slot())?;
    cell.read().hint.clone()
}

/// The hint as the status bar's right-side override line.
///
/// Warning-colored so it reads as a notice rather than ambient info.
fn hint_override_line(hint: &str, theme: &Theme) -> Line<'static> {
    Line::from(Span::styled(
        hint.to_owned(),
        Style::default().fg(theme.warning),
    ))
}

/// The provider slice's model cache, if the slice is attached.
///
/// App-level, not per-session: one provider catalog shared by every
/// session. Exported because the shared info line takes the cache as a
/// parameter, and a caller with an `AppState` in hand (the sidebar's
/// production render path) resolves it the same way the status bar does.
#[must_use]
pub fn model_cache_of(state: &AppState) -> Option<ModelCache> {
    state.provider_state()?.read().model_cache.clone()
}

/// Foreground style for a displayed cache-hit percentage, banded by health:
/// 95%+ healthy (`success`), 90–94% degraded (`warning`), below 90% poor
/// (`error_text`). Bands apply to the rounded display value, so what the
/// user reads is what gets colored.
fn cache_hit_style(theme: &Theme, pct: u32) -> Style {
    let color = match pct {
        p if p >= 95 => theme.success,
        p if p >= 90 => theme.warning,
        _ => theme.error_text,
    };
    Style::default().fg(color)
}
