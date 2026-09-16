//! Status bar - displays the session CWD, active prompt strategy, and current model.
//!
//! Shows the session's working directory on line 1, and status information
//! on line 2: strategy, pinned count, token stats, turn count, and model.
//! The model shows `({provider})/{model}` when set, or "no model selected" otherwise.

use jinn_domain::common::app_state::AppState;
use jinn_domain::common::path_display::shorten_path;
use jinn_domain::common::render_ctx::RenderCtx;
use jinn_domain::common::ui_element::UiElement;
use jinn_domain::feat::provider_infra::InputModalities;
use jinn_domain::feat::provider_infra::ModelCache;
use jinn_domain::feat::provider_infra::ModelInfo;
use jinn_domain::feat::session::aggregate_tree_stats;
use jinn_domain::feat::session::model_selection::ModelSelection;
use jinn_domain::feat::session::token_stats::TokenStats;
use jinn_domain::resolve_effort;
use jinn_theme::Theme;
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::turn_counter;
use jinn_status_bar_msg::StatusBarState;
use jinn_status_bar_msg::status_bar_slot;

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

        render_cwd_line(frame, cwd_area, state, style);
        render_tree_aggregate(frame, cwd_area, state, style);
        render_token_info_line(frame, info_area, state, ctx, style);
    }
}

/// Renders the CWD line (left-aligned) for the active session.
fn render_cwd_line(frame: &mut Frame<'_>, area: Rect, state: &AppState, style: Style) {
    let cwd = state.active_session().cwd();
    let cwd_display = shorten_path(cwd);
    let cwd_widget = Paragraph::new(Line::from(Span::styled(cwd_display, style)))
        .style(style)
        .alignment(Alignment::Left);
    frame.render_widget(cwd_widget, area);
}

/// Renders the tree aggregate (right-aligned on the CWD line) when the tree has >1 session.
fn render_tree_aggregate(frame: &mut Frame<'_>, area: Rect, state: &AppState, style: Style) {
    let tree = aggregate_tree_stats(
        state.session.sessions(),
        state.session.frozen_nodes(),
        state.session.active_session_id(),
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
    tree_spans.push(Span::styled(format!("{turn_symbol}{turns} "), style));
    tree_spans.push(Span::styled(format!("{session_symbol}{count}"), style));
    let tree_widget = Paragraph::new(Line::from(tree_spans)).alignment(Alignment::Right);
    frame.render_widget(tree_widget, area);
}

/// Renders the token/model info line (left: token stats + cost + turns; right: model).
fn render_token_info_line(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ctx: &RenderCtx,
    style: Style,
) {
    let active_model = state.active_session().profile().model.clone();
    let token_stats = TokenStats::from_ledger(state.active_session().token_ledger());
    let token_info = build_token_info_string(state, &active_model, &token_stats);

    let model = build_model_string(state, &active_model);
    let total_cost = TokenStats::total_cost(state.active_session().token_ledger());

    let left_side = {
        let turn_count = turn_counter::compute_turn_count(
            state.active_session().history(),
            state.active_session().fork_ordinal(),
        );
        let turn_symbol = '\u{21BB}';
        let mut left_spans: Vec<Span> = Vec::new();
        // Cache-hit percentage — leftmost, shown only when there are cache hits.
        if let Some(pct) = cache_hit_percent(&token_stats) {
            let cache_glyph = '\u{2B22}'; // ⬢
            left_spans.push(Span::styled(
                format!("{cache_glyph} {pct}% "),
                cache_hit_style(&state.frontend.theme, pct),
            ));
        }
        left_spans.push(Span::styled(token_info, style));
        left_spans.push(Span::styled(format!(" ${:.5}", total_cost.abs()), style));
        left_spans.push(Span::styled(format!(" {turn_symbol}{turn_count}"), style));
        Paragraph::new(Line::from(left_spans))
            .style(style)
            .alignment(Alignment::Left)
    };
    frame.render_widget(left_side, area);

    // A transient hint (e.g. an inert overlay toggle) replaces the model
    // display until the next intent; it is warning-colored so it reads as a
    // notice rather than ambient info. The hint lives in the slice's cell —
    // absent cell (slice not activated) means no hint, model shows.
    let hint = ctx
        .slices
        .reader::<StatusBarState>(&status_bar_slot())
        .and_then(|cell| cell.read().hint.clone());
    let right_side = if let Some(hint) = hint {
        let hint_style = Style::default().fg(state.frontend.theme.warning);
        Paragraph::new(Line::from(vec![Span::styled(hint, hint_style)])).alignment(Alignment::Right)
    } else {
        let right_spans = vec![Span::styled(model, style)];
        Paragraph::new(Line::from(right_spans)).alignment(Alignment::Right)
    };
    frame.render_widget(right_side, area);
}

/// Builds the left-side token info string: sent/received counts + context budget.
fn build_token_info_string(
    state: &AppState,
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

    let ctx_size = state.active_session().context_size();
    let ctx_limit = resolve_context_limit(
        state.provider.model_cache.as_ref(),
        active_model.display_str(),
    );
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
fn build_model_string(state: &AppState, active_model: &ModelSelection) -> String {
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
            state
                .active_session()
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
    let resolved_effort = resolve_effort(state.active_session().profile().reasoning_effort);
    let model = match resolved_effort {
        Some(effort) => format!("{model} [{}]", effort.as_str()),
        None => model,
    };

    // Append the modality indicator as `<t>` / `<ti>` AFTER the effort bracket,
    // using the SAME resolved model the name surfaces (last-dispatched for
    // alloys, else selection). Conservative: an unknown / not-in-cache model
    // shows `<t>` (text is always available); "no model selected" shows nothing.
    let modalities = resolved_for_lookup.map(|resolved| {
        resolve_modalities(state.provider.model_cache.as_ref(), &resolved)
            .unwrap_or_else(InputModalities::text)
    });
    match modalities {
        Some(m) => format!("{model} <{}>", m.display()),
        None => model,
    }
}
