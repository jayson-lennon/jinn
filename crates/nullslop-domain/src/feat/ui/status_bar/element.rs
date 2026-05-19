//! Status bar — displays the session CWD, active prompt strategy, and current model.
//!
//! Shows the session's working directory on line 1, and status information
//! on line 2: strategy, pinned count, token stats, turn count, and model.
//! The model shows `({provider})/{model}` when set, or "no model selected" otherwise.

use crate::common::app_state::AppState;
use crate::common::ui_element::UiElement;
use crate::feat::provider_infra::NO_PROVIDER_ID;
use crate::feat::session::aggregate_session_stats;
use crate::feat::session::chat_session::SessionPhase;
use crate::feat::ui::status_bar::turn_counter;
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

/// A display element that shows the active strategy and provider/model in the status bar.
#[derive(Debug)]
pub struct StatusBarElement;

/// Shorten a path for display: resolve `.` to absolute, replace home with `~`.
fn shorten_path(path: &std::path::Path) -> String {
    let absolute = if path == std::path::Path::new(".") {
        std::env::current_dir().unwrap_or_else(|_| path.to_path_buf())
    } else {
        path.to_path_buf()
    };
    if let Some(home) = dirs::home_dir()
        && let Ok(relative) = absolute.strip_prefix(&home)
    {
        let display = relative.display().to_string();
        if display.is_empty() {
            return "~".to_owned();
        }
        return format!("~/{display}");
    }
    absolute.display().to_string()
}

/// Format a token count in human-readable form with one decimal place.
#[allow(clippy::cast_precision_loss)]
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

impl UiElement<AppState> for StatusBarElement {
    fn name(&self) -> String {
        "status-bar".to_owned()
    }

    #[allow(clippy::cast_precision_loss, clippy::too_many_lines)]
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, state: &AppState) {
        // Split area into cwd line + info line.
        let [cwd_area, info_area] =
            Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(area);

        // --- Line 1: CWD ---
        let cwd = state.active_session().cwd();
        let cwd_display = shorten_path(cwd);
        let style = Style::default().fg(state.frontend.theme.muted_text);
        let cwd_widget = Paragraph::new(Line::from(Span::styled(cwd_display, style)))
            .style(style)
            .alignment(Alignment::Left);
        frame.render_widget(cwd_widget, cwd_area);

        // --- Line 2: Existing info ---
        let strategy = state.active_session().active_strategy();
        let pinned_count = state.active_session().pinned_entries().len();
        let active_model = state.active_session().profile().model.clone();

        // Compute aggregated token stats for the active session.
        let agg =
            aggregate_session_stats(state.session.sessions(), state.session.active_session_id());
        let up_arrow = '\u{2191}';
        let down_arrow = '\u{2193}';
        let mut token_info = format!(
            "{up_arrow}{} {down_arrow}{}",
            format_tokens(agg.total_sent()),
            format_tokens(agg.total_received()),
        );

        let context_display = if let Some(ctx_size) = state.active_session().context_size() {
            let ctx_used = u64::from(ctx_size);
            let ctx_limit = state.provider.model_cache.as_ref().and_then(|cache| {
                // active_model is "provider/model" — extract provider name.
                let provider_name = active_model.split('/').next()?;
                let models = cache.entries.get(provider_name)?;
                // Find the model matching the full ID.
                let model_suffix = &active_model[(provider_name.len() + 1)..];
                models
                    .iter()
                    .find(|m| m.id == model_suffix)
                    .and_then(|m| m.context_length)
            });

            if let Some(max_tokens) = ctx_limit {
                let max_u64 = u64::from(max_tokens);
                let pct = if max_u64 > 0 {
                    format!("{:.1}%", (ctx_used as f64 / max_u64 as f64) * 100.0)
                } else {
                    "0.0%".to_owned()
                };
                format!("{}/{}", pct, format_budget(max_tokens as usize))
            } else {
                format!("{}/???", format_tokens(ctx_used))
            }
        } else {
            "0/???".to_owned()
        };
        token_info = format!("{token_info} {context_display}");

        let strategy_display =
            if state.active_session().active_strategy().as_str() == "token_budget" {
                let budget = state.active_session().profile().token_budget;
                format!("Token Budget: {}", format_budget(budget))
            } else {
                strategy.to_string()
            };

        let left = if pinned_count > 0 {
            format!("({strategy_display})\u{1f4cc}{pinned_count} {token_info}")
        } else {
            format!("({strategy_display}) {token_info}")
        };

        let model = if active_model == NO_PROVIDER_ID {
            "no model selected".to_owned()
        } else if let Some((provider, model)) = active_model.split_once('/') {
            format!("({provider})/{model}")
        } else {
            active_model.clone()
        };

        // Build left side: strategy info + cost + turn count.
        let total_cost = agg.total_cost();
        let turn_count = turn_counter::compute_turn_count(state.active_session().history());
        let left_spans: Vec<Span> = vec![
            Span::styled(left, style),
            Span::styled(format!(" ${total_cost:.4}"), style),
            Span::styled(format!(" Turns: {turn_count}"), style),
        ];

        let strategy_widget = Paragraph::new(Line::from(left_spans))
            .style(style)
            .alignment(Alignment::Left);
        frame.render_widget(strategy_widget, info_area);

        let notification = state.frontend.active_status_notification();
        let right_spans = if matches!(state.active_session().phase(), SessionPhase::Compacting) {
            // Show "Compacting..." prominently when session is in Compacting phase.
            vec![
                Span::styled(
                    "Compacting...",
                    Style::default().fg(state.frontend.theme.warning),
                ),
                Span::styled(format!("  {model}"), style),
            ]
        } else if let Some(msg) = notification {
            vec![
                Span::styled(msg, Style::default().fg(state.frontend.theme.success)),
                Span::styled(format!("  {model}"), style),
            ]
        } else {
            vec![Span::styled(model, style)]
        };
        let right_line = Line::from(right_spans);
        let model_widget = Paragraph::new(right_line).alignment(Alignment::Right);
        frame.render_widget(model_widget, info_area);
    }
}
