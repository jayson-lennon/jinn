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
use jinn_core_types::model_selection::ModelSelection;
use jinn_core_types::{SessionId, WorkingInterval, format_working_duration};
use jinn_kernel::common::app_state::AppState;
use jinn_kernel::common::render_ctx::RenderCtx;
use jinn_kernel::common::ui_element::UiElement;
use jinn_kernel::resolve_effort;
use jinn_provider_config::InputModalities;
use jinn_provider_config::ModelCache;
use jinn_provider_config::ModelInfo;
use jinn_session_state::ChatSessionState;
use jinn_session_state::aggregate_tree_stats;
use jinn_session_state::compute_turn_count;
use jinn_slices::DrawContext;
use jinn_status_bar_msg::StatusBarState;
use jinn_status_bar_msg::status_bar_slot;
use jinn_theme::Theme;
use jinn_token_count_msg::TokenStats;
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

/// Format a token count in human-readable form with one decimal place.
#[expect(
    clippy::cast_precision_loss,
    reason = "loss is negligible for UI calculations"
)]
fn format_tokens(count: u64) -> String {
    if count >= 1_000_000 {
        format!("{:.1}M", count as f64 / 1_000_000.0)
    } else if count >= 1_000 {
        format!("{:.1}k", count as f64 / 1_000.0)
    } else {
        count.to_string()
    }
}

/// Format a token budget as whole numbers only (e.g. `150k`, `1M`, `999`).
fn format_budget(count: usize) -> String {
    if count >= 1_000_000 {
        format!("{}M", count / 1_000_000)
    } else if count >= 1_000 {
        format!("{}k", count / 1_000)
    } else {
        count.to_string()
    }
}

/// Resolve the active model's `ModelInfo` from the cache.
///
/// `active_model` must be the `provider/model-id` display form (i.e. the same
/// string the status bar surfaces as the model name). Returns `None` when the
/// cache is absent or the model is not recorded in it.
fn resolve_model_info<'a>(
    model_cache: Option<&'a ModelCache>,
    active_model: &str,
) -> Option<&'a ModelInfo> {
    let cache = model_cache?;
    let provider_name = active_model.split('/').next()?;
    let models = cache.entries.get(provider_name)?;
    let model_suffix = active_model.get((provider_name.len() + 1)..)?;
    models.iter().find(|m| m.id == model_suffix)
}

/// Look up the context_length for the active model from the model cache.
///
/// Returns `Some(context_length)` if the cache is populated and the active model
/// is found with a non-None context_length. Returns `None` otherwise.
fn resolve_context_limit(model_cache: Option<&ModelCache>, active_model: &str) -> Option<u32> {
    let info = resolve_model_info(model_cache, active_model)?;
    info.context_length
}

/// Resolve the active model's input modalities from the cache.
///
/// Returns the cached modalities when the model is recorded, otherwise `None`
/// (so the caller can apply the conservative text-only default).
fn resolve_modalities(
    model_cache: Option<&ModelCache>,
    active_model: &str,
) -> Option<InputModalities> {
    resolve_model_info(model_cache, active_model).map(|m| m.input_modalities)
}

/// The context size the shared info line displays for a session.
///
/// A session's cached assembled size is only written for the active session —
/// the context-size actor recalculates `active_session_id()` on
/// context-affecting events — so a background session read straight from the
/// session map carries `None` and would render `0.0%/<max>` even though it has
/// dispatched turns. The last token-ledger record is the fallback: both
/// dispatch writers push the dispatch's `assembled.estimated_tokens()` into
/// `tokens_sent`, which is the same per-dispatch whole-context estimate the
/// actor caches (an estimate of that request's entire prompt, not an increment
/// — so it must be read from one record, never summed across the ledger).
///
/// Cached wins when present: the actor re-assembles with pins, overrides, and
/// pruning applied, which the raw dispatch estimate does not reflect. A
/// session with no cached size and an empty ledger has genuinely never
/// dispatched, so `None` — and the zero display is correct.
fn displayed_context_size(session: &ChatSessionState) -> Option<u32> {
    session.context_size().or_else(|| {
        session
            .token_ledger()
            .last()
            .map(|record| record.tokens_sent)
    })
}

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
    let tree = aggregate_tree_stats(
        state.session.sessions(),
        state.session.frozen_nodes(),
        state.session.active_session_id(),
        &live_working_intervals(slices),
    );
    if tree.session_count <= 1 {
        return;
    }
    let up_arrow = '\u{2191}';
    let down_arrow = '\u{2193}';
    let turn_symbol = '\u{21BB}';
    let session_symbol = '\u{29C9}';
    let tree_prefix = '\u{1F333}';
    let turns = tree.total_turns;
    let count = tree.session_count;
    let mut tree_spans: Vec<Span> = vec![Span::styled(format!("{tree_prefix} "), style)];
    match cache_hit_percent_from(tree.cached_total, tree.measured_sent) {
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
        format!("{up_arrow}{} ", format_tokens(tree.effective_sent)),
        style,
    ));
    tree_spans.push(Span::styled(
        format!("{down_arrow}{} ", format_tokens(tree.total_received)),
        style,
    ));
    tree_spans.push(Span::styled(format!("${:.5} ", tree.total_cost), style));
    tree_spans.push(Span::styled(format!("{turn_symbol}{turns}"), style));
    tree_spans.push(Span::styled(
        format!(" {}", format_working_duration(&tree.total_working)),
        style,
    ));
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
    let active_model = session.profile().model.clone();
    let token_stats = TokenStats::from_ledger(session.token_ledger());
    let token_info = build_token_info_string(model_cache, session, &active_model, &token_stats);

    let model = build_model_string(model_cache, session, &active_model);
    let total_cost = TokenStats::total_cost(session.token_ledger());

    let left_side = {
        let turn_count = compute_turn_count(session.history(), session.fork_ordinal());
        let turn_symbol = '\u{21BB}';
        let mut left_spans: Vec<Span> = Vec::new();
        // Cache-hit percentage — leftmost, shown only when there are cache hits.
        if let Some(pct) = cache_hit_percent(&token_stats) {
            let cache_glyph = '\u{2B22}'; // ⬢
            left_spans.push(Span::styled(
                format!("{cache_glyph} {pct}% "),
                cache_hit_style(theme, pct),
            ));
        }
        left_spans.push(Span::styled(token_info, style));
        left_spans.push(Span::styled(format!(" ${:.5}", total_cost.abs()), style));
        left_spans.push(Span::styled(
            format!(
                " {turn_symbol}{turn_count} {}",
                format_working_duration(&working_time)
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
    let right_line = right_override.unwrap_or_else(|| Line::from(Span::styled(model, style)));
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

fn build_token_info_string(
    model_cache: Option<&ModelCache>,
    session: &ChatSessionState,
    active_model: &ModelSelection,
    token_stats: &TokenStats,
) -> String {
    let up_arrow = '\u{2191}';
    let down_arrow = '\u{2193}';
    let token_info = format!(
        "{up_arrow}{} {down_arrow}{}",
        format_tokens(token_stats.effective_sent),
        format_tokens(token_stats.total_received),
    );

    let ctx_size = displayed_context_size(session);
    let ctx_limit = resolve_context_limit(model_cache, active_model.display_str());
    let context_display = match (ctx_size, ctx_limit) {
        (Some(used), Some(max)) => {
            let ctx_used = u64::from(used);
            let max_u64 = u64::from(max);
            let pct = if max_u64 > 0 {
                format!("{:.1}%", (ctx_used as f64 / max_u64 as f64) * 100.0)
            } else {
                "0.0%".to_owned()
            };
            format!("{}/{}", pct, format_budget(max as usize))
        }
        (None, Some(max)) => format!("0.0%/{}", format_budget(max as usize)),
        (Some(used), None) => format!("{}/???", format_tokens(u64::from(used))),
        (None, None) => "0/???".to_owned(),
    };
    format!("{token_info} {context_display}")
}

/// Cache-hit percentage for a session ledger, or `None` when there are no
/// cache hits. The denominator is `measured_sent` (provider-reported
/// `prompt_tokens` over turns that reported usage), so cancelled turns — which
/// report no usage — are excluded from both numerator and denominator.
fn cache_hit_percent(stats: &TokenStats) -> Option<u32> {
    cache_hit_percent_from(stats.cached_total, stats.measured_sent)
}

/// Cache-hit percentage over measured turns only, computed from raw sums.
///
/// Returns `None` when there are no cache hits or no measured turns, so the
/// glyph stays hidden for providers/turns that report no usage.
fn cache_hit_percent_from(cached_total: u64, measured_sent: u64) -> Option<u32> {
    if cached_total == 0 || measured_sent == 0 {
        return None;
    }
    let pct = (cached_total as f64 / measured_sent as f64) * 100.0;
    Some(pct.round() as u32)
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

/// Builds the right-side model display string: resolved name + reasoning effort.
fn build_model_string(
    model_cache: Option<&ModelCache>,
    session: &ChatSessionState,
    active_model: &ModelSelection,
) -> String {
    let (model, resolved_for_lookup) = {
        // For an Alloy, the bar surfaces the last-rotated member (which one
        // actually answered) from the token ledger, falling back to the first
        // member before any dispatch. For a Single selection the model _is_ the
        // source of truth — never read the historical ledger, which would show
        // a stale model after switching providers via the picker.
        //
        // `model_used` is cloned to an owned `Option<String>` so the resolved
        // `&str` borrows only `active_model` and a local, avoiding a lifetime
        // clash between the ledger borrow and the `active_model` borrow.
        let last_dispatched = active_model.as_alloy().and_then(|_| {
            session
                .token_ledger()
                .last()
                .and_then(|r| r.model_used.clone())
        });
        let resolved = last_dispatched
            .as_deref()
            .unwrap_or_else(|| active_model.display_str());

        if active_model.is_no_provider() {
            ("no model selected".to_owned(), None)
        } else {
            let display = if let Some((provider, m)) = resolved.split_once('/') {
                format!("({provider})/{m}")
            } else {
                resolved.to_owned()
            };
            (display, Some(resolved.to_owned()))
        }
    };

    let model = match active_model.as_alloy() {
        Some(alloy) => format!("[alloy {}] {model}", alloy.models.len()),
        None => model,
    };

    // Append the resolved reasoning effort as `[<mode>]`.
    // Omitted entirely when no effort is resolved (no `[none]` noise).
    let resolved_effort = resolve_effort(session.profile().reasoning_effort);
    let model = match resolved_effort {
        Some(effort) => format!("{model} [{}]", effort.as_str()),
        None => model,
    };

    // Append the modality indicator as `<t>` / `<ti>` AFTER the effort bracket,
    // using the SAME resolved model the name surfaces (last-dispatched for
    // alloys, else selection). Conservative: an unknown / not-in-cache model
    // shows `<t>` (text is always available); "no model selected" shows nothing.
    let modalities = resolved_for_lookup.map(|resolved| {
        resolve_modalities(model_cache, &resolved).unwrap_or_else(InputModalities::text)
    });
    match modalities {
        Some(m) => format!("{model} <{}>", m.display()),
        None => model,
    }
}
