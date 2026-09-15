//! The OpenRouter endpoint picker's spec — behavior authored once in the builder.
//!
//! Open signals the provider actor to load (or refresh) the upstream list —
//! the load performs a network fetch behind an in-memory cache, so it stays
//! an async round-trip. CTRL+R forces a cache-bypassing refresh. Enter pins
//! the selected upstream on the session profile; the auto-route sentinel
//! clears the pin.

use jinn_picker::ActionCtx;
use jinn_picker::PickerId;
use jinn_picker::PickerOutcome;
use jinn_picker::PickerSpec;
use jinn_picker::PickerWidget;
use jinn_picker::PreviewCtx;
use jinn_picker::PreviewSpec;
use jinn_picker::RowCtx;
use jinn_picker::StatusCtx;
use jinn_selection_widget::highlight_text_with_bg;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::text::Span;

use crate::common::app_state::AppState;
use crate::feat::endpoint::Endpoint;
use crate::feat::endpoint::picker_entry::EndpointEntry;
use crate::feat::provider::protocol::command::LoadEndpointPickerEntries;
use crate::feat::provider::protocol::command::RefreshEndpointPickerEntries;
use crate::feat::session::model_selection::ModelSelection;
use crate::feat::session::protocol::mark_session_interacted::MarkSessionInteracted;
use crate::feat::ui::picker_states::PickerExt;

/// Builds the endpoint picker's spec.
#[must_use]
pub fn endpoint_spec() -> PickerSpec<EndpointEntry> {
    PickerSpec::new(PickerId::new(crate::feat::picker::registry::ENDPOINT_ID))
        .title(" OpenRouter Endpoint ")
        .widget(PickerWidget::Preview(PreviewSpec {
            reset_scroll_on_selection_change: false,
        }))
        .row(endpoint_row)
        .search(|entry| {
            if entry.tag.is_empty() {
                entry.provider_name.clone()
            } else {
                format!("{} {}", entry.provider_name, entry.tag)
            }
        })
        .preview(endpoint_preview)
        .preview_key(|entry: &EndpointEntry| {
            // Metadata is static per entry; cache by tag so the preview pane
            // doesn't re-render every frame.
            (!entry.tag.is_empty()).then(|| jinn_picker::PreviewKey(entry.tag.clone()))
        })
        .bind("<c-r>", "refresh", refresh_endpoints)
        .on_open(open_endpoint)
        .on_confirm(confirm_endpoint)
        .status(endpoint_status)
}

/// The domain state behind an [`ActionCtx`]. The kernel's host lens always
/// lends `AppState`; this downcast is the spec's single sanctioned escape.
fn state_of<'a>(ctx: &'a mut ActionCtx<'_>) -> &'a mut AppState {
    ctx.state_any()
        .downcast_mut::<AppState>()
        .expect("domain host lends AppState")
}

/// The read-only domain state behind a [`StatusCtx`].
fn state_ref_of<'a>(ctx: &'a StatusCtx<'_>) -> &'a AppState {
    ctx.state_any_ref()
        .downcast_ref::<AppState>()
        .expect("domain host lends AppState")
}

// ── Rendering ────────────────────────────────────────────────────────────

/// Renders one picker row: `● name  (tag)` with the active marker bold.
/// Filter matches highlight within the name.
fn endpoint_row(entry: &EndpointEntry, ctx: &RowCtx<'_>) -> Line<'static> {
    let theme = &entry.theme;
    let active_marker = Span::styled(
        if entry.is_active { "\u{25cf} " } else { "  " },
        if entry.is_active {
            Style::default()
                .fg(theme.picker_active_marker)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        },
    );

    let label_style = if ctx.is_selected {
        Style::default()
            .fg(theme.primary_text)
            .bg(theme.picker_selected_bg)
    } else {
        Style::default()
    };

    let suffix = if entry.tag.is_empty() {
        "(auto-route)".to_owned()
    } else {
        format!("({})", entry.tag)
    };

    let name_spans = if ctx.match_ranges.is_empty() {
        vec![Span::styled(
            format!("{}  ", entry.provider_name),
            label_style,
        )]
    } else {
        let mut spans = highlight_text_with_bg(
            &entry.provider_name,
            label_style,
            ctx.match_ranges,
            theme.picker_highlight_bg,
        );
        spans.push(Span::styled("  ".to_owned(), label_style));
        spans
    };

    let mut all_spans = vec![active_marker];
    all_spans.extend(name_spans);
    all_spans.push(Span::styled(suffix, label_style));
    Line::from(all_spans)
}

/// Renders the preview pane for the selected endpoint: its routing tag,
/// uptime, quantization, and pricing — or a one-line explanation for the
/// auto-route sentinel.
fn endpoint_preview(entry: &EndpointEntry, _ctx: &PreviewCtx<'_>) -> Vec<Line<'static>> {
    // The auto-route sentinel has no metadata; show a one-line explanation.
    if entry.tag.is_empty() {
        return vec![
            Line::from("Let OpenRouter choose the upstream each turn.")
                .style(Style::default().fg(entry.theme.muted_text)),
        ];
    }

    let gray = Style::default().fg(entry.theme.muted_text);
    let primary = Style::default().fg(entry.theme.primary_text);
    let row = |label: &str, value: &str| {
        Line::from(vec![
            Span::styled(format!("{label}: "), gray),
            Span::styled(value.to_owned(), primary),
        ])
    };

    let uptime = entry
        .uptime_30m
        .map_or_else(|| "unknown".to_owned(), |u| format!("{u:.1}%"));
    let quant = entry
        .quantization
        .clone()
        .unwrap_or_else(|| "unknown".to_owned());
    let prompt = entry
        .prompt_price
        .clone()
        .unwrap_or_else(|| "unknown".to_owned());
    let completion = entry
        .completion_price
        .clone()
        .unwrap_or_else(|| "unknown".to_owned());
    let max_tokens = entry
        .max_completion_tokens
        .map_or_else(|| "unknown".to_owned(), |n| n.to_string());

    vec![
        row("Tag", &entry.tag),
        row("Uptime (30m)", &uptime),
        row("Quantization", &quant),
        row("Prompt price", &prompt),
        row("Completion price", &completion),
        row("Max completion", &max_tokens),
    ]
}

/// The status line: the pinned upstream (or auto-route) plus the fetch
/// state — a spinner-style indicator while a fetch is in flight, otherwise
/// the cache's age.
#[expect(clippy::unnecessary_wraps, reason = "hook signature is Option<Line>")]
fn endpoint_status(ctx: &StatusCtx<'_>) -> Option<Line<'static>> {
    let state = state_ref_of(ctx);
    let gray = Style::default().fg(state.frontend.theme.muted_text);
    let orange = Style::default().fg(state.frontend.theme.accent_action);

    let pinned_name = state
        .frontend
        .endpoint_picker()
        .items()
        .iter()
        .find(|e| e.entry().is_active)
        .map_or("auto-route", |e| e.entry().provider_name.as_str());

    let mut spans = vec![
        Span::styled("Routing: ".to_owned(), gray),
        Span::styled(
            pinned_name.to_owned(),
            Style::default().fg(state.frontend.theme.primary_text),
        ),
        Span::styled("  ".to_owned(), gray),
    ];

    if state.frontend.pickers.endpoint_loading {
        spans.push(Span::styled("fetching\u{2026}".to_owned(), orange));
    } else if let Some(ts) = state.frontend.pickers.endpoint_fetched_at {
        spans.push(Span::styled(format!("fetched {}", format_age(ts)), gray));
    }

    Some(Line::from(spans))
}

/// Coarse "time ago" formatter for the endpoint cache freshness line.
///
/// `<60s` → `Xs`, `<60m` → `Xm`, else `Xh`.
fn format_age(fetched_at: jiff::Timestamp) -> String {
    let elapsed = jiff::Timestamp::now() - fetched_at;
    let secs = elapsed.total(jiff::Unit::Second).unwrap_or(0.0).max(0.0) as u64;
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 60 * 60 {
        format!("{}m ago", secs / 60)
    } else {
        format!("{}h ago", secs / 3600)
    }
}

// ── Lifecycle ────────────────────────────────────────────────────────────

/// Opening the endpoint picker: fresh filter + selection, flag the fetch as
/// in-flight, and signal the provider actor to load entries (it owns the
/// network fetch and the cache).
fn open_endpoint(ctx: &mut ActionCtx<'_>) -> PickerOutcome {
    let state = state_of(ctx);
    state.frontend.endpoint_picker_mut().reset();
    state.frontend.pickers.endpoint_loading = true;
    PickerOutcome::empty().with_message(LoadEndpointPickerEntries)
}

/// CTRL+R on the endpoint picker: force a cache-bypassing refresh. A no-op
/// for alloys (the endpoint picker does not apply to them).
fn refresh_endpoints(ctx: &mut ActionCtx<'_>) -> PickerOutcome {
    let state = state_of(ctx);
    if matches!(
        state.active_session().profile().model,
        ModelSelection::Alloy { .. }
    ) {
        return PickerOutcome::empty();
    }
    // Flag the fetch as in-flight this frame, reset, and publish the
    // forced-refresh command.
    state.frontend.pickers.endpoint_loading = true;
    state.frontend.endpoint_picker_mut().reset();
    PickerOutcome::empty().with_message(RefreshEndpointPickerEntries)
}

/// Enter on the endpoint picker: pin the selected upstream on the session
/// profile (the auto-route sentinel clears the pin) and persist immediately.
fn confirm_endpoint(ctx: &mut ActionCtx<'_>) -> PickerOutcome {
    let Some(entry) = state_of(ctx)
        .frontend
        .endpoint_picker()
        .selected_item()
        .cloned()
    else {
        return PickerOutcome::empty();
    };
    let endpoint = if entry.entry().tag.is_empty() {
        None
    } else {
        Some(Endpoint {
            tag: entry.entry().tag.clone(),
            provider_name: entry.entry().provider_name.clone(),
        })
    };
    let state = state_of(ctx);
    let session_id = state.session.active_session_id().clone();

    state.active_session_mut().profile_mut().endpoint = endpoint;

    PickerOutcome::empty()
        .with_message(MarkSessionInteracted { session_id })
        .close()
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

    use super::*;
    use crate::feat::picker::host_impl::AppStatePickerHost;
    use crate::feat::session::ChatSessionState;
    use crate::feat::theme::default_theme;
    /// AppState with an active session on a single OpenRouter model.
    fn single_model_state() -> AppState {
        let mut state = AppState::default();
        let origin = ChatSessionState::new();
        state.session.insert(origin);
        state
            .session
            .set_active(state.session.active_session_id().clone());
        state.active_session_mut().set_model(ModelSelection::Single(
            "openrouter/anthropic/claude-sonnet-4".to_owned(),
        ));
        state
    }

    /// Wraps raw entries through the registered spec's hooks.
    fn wrap(entries: Vec<EndpointEntry>) -> Vec<jinn_picker::PickerEntry<EndpointEntry>> {
        crate::feat::picker::registry::build_picker_registry()
            .make_items(crate::feat::picker::registry::ENDPOINT_ID, entries)
            .expect("endpoint spec is registered")
    }

    /// Runs a spec hook against `state` with a fresh dispatch context.
    fn run(
        state: &mut AppState,
        f: impl FnOnce(&mut ActionCtx<'_>) -> PickerOutcome,
    ) -> PickerOutcome {
        let mut host = AppStatePickerHost::new(state);
        let mut ctx = ActionCtx::new(
            PickerId::new(crate::feat::picker::registry::ENDPOINT_ID),
            &mut host,
        );
        f(&mut ctx)
    }

    // ── Lifecycle ─────────────────────────────────────────────────────

    #[rstest::rstest]
    #[test]
    fn open_sets_loading_and_emits_load() {
        // Given a session on a single model.
        let mut state = single_model_state();

        // When opening the endpoint picker through the spec.
        let outcome = run(&mut state, open_endpoint);

        // Then loading is flagged this frame and the load message is emitted.
        assert!(
            state.frontend.pickers.endpoint_loading,
            "open must set loading so the indicator appears this frame"
        );
        assert!(
            outcome
                .message_names
                .iter()
                .any(|n| n.contains("LoadEndpointPickerEntries")),
            "open should emit LoadEndpointPickerEntries: {:?}",
            outcome.message_names
        );
    }

    // ── CTRL+R refresh ────────────────────────────────────────────────

    #[rstest::rstest]
    #[test]
    fn ctrl_r_sets_loading_and_emits_refresh() {
        // Given a session on a single model.
        let mut state = single_model_state();

        // When pressing CTRL+R.
        let outcome = run(&mut state, refresh_endpoints);

        // Then loading is flagged and the forced-refresh command is emitted.
        assert!(
            state.frontend.pickers.endpoint_loading,
            "refresh must set loading so the indicator appears this frame"
        );
        assert!(
            outcome
                .message_names
                .iter()
                .any(|n| n.ends_with("RefreshEndpointPickerEntries")),
            "refresh must emit RefreshEndpointPickerEntries: {:?}",
            outcome.message_names
        );
    }

    #[rstest::rstest]
    #[test]
    fn ctrl_r_is_a_noop_for_an_alloy_model() {
        // Given a session on an alloy of two models.
        let mut state = single_model_state();
        state.active_session_mut().set_model(ModelSelection::Alloy {
            models: vec!["ollama/llama3".to_owned(), "ollama/mistral".to_owned()],
            strategy: crate::feat::session::model_selection::AlloyStrategy::RoundRobin { index: 0 },
        });

        // When pressing CTRL+R.
        let outcome = run(&mut state, refresh_endpoints);

        // Then it is a no-op: no command, loading never set.
        assert!(
            outcome.message_names.is_empty(),
            "refresh must be a no-op for an alloy model"
        );
        assert!(
            !state.frontend.pickers.endpoint_loading,
            "refresh must not set loading for an alloy model"
        );
    }

    // ── Confirm ───────────────────────────────────────────────────────

    #[rstest::rstest]
    #[test]
    fn confirm_pins_the_selected_endpoint_and_persists() {
        // Given a picker with the Anthropic upstream highlighted.
        let mut state = single_model_state();
        let entry = EndpointEntry {
            tag: "anthropic".to_owned(),
            provider_name: "Anthropic".to_owned(),
            uptime_30m: None,
            prompt_price: None,
            completion_price: None,
            quantization: None,
            max_completion_tokens: None,
            is_active: false,
            theme: default_theme(),
        };
        state
            .frontend
            .endpoint_picker_mut()
            .set_items(wrap(vec![entry]));
        state.frontend.endpoint_picker_mut().move_down(1);

        // When confirming through the spec.
        let outcome = run(&mut state, confirm_endpoint);

        // Then the session profile pins the Anthropic endpoint.
        let pinned = state.active_session().profile().endpoint.clone();
        assert_eq!(pinned.map(|e| e.tag), Some("anthropic".to_owned()));
        // And MarkSessionInteracted is emitted and the picker closes.
        assert!(
            outcome
                .message_names
                .iter()
                .any(|n| n.contains("MarkSessionInteracted")),
            "confirm must persist the session: {:?}",
            outcome.message_names
        );
        assert!(outcome.close, "confirm should close the picker");
    }

    #[rstest::rstest]
    #[test]
    fn confirm_sentinel_clears_the_pin() {
        // Given a session that already has a pinned endpoint and the
        // auto-route sentinel highlighted.
        let mut state = single_model_state();
        state.active_session_mut().profile_mut().endpoint = Some(Endpoint {
            tag: "anthropic".to_owned(),
            provider_name: "Anthropic".to_owned(),
        });
        state
            .frontend
            .endpoint_picker_mut()
            .set_items(wrap(vec![EndpointEntry::auto_route(true, default_theme())]));

        // When confirming the sentinel through the spec.
        let _ = run(&mut state, confirm_endpoint);

        // Then the pin is cleared.
        assert!(
            state.active_session().profile().endpoint.is_none(),
            "selecting the auto-route sentinel must clear the pin"
        );
    }

    // ── Rendering ─────────────────────────────────────────────────────

    #[rstest::rstest]
    #[test]
    fn preview_shows_sentinel_line_for_auto_route() {
        // Given the auto-route sentinel.
        let entry = EndpointEntry::auto_route(false, default_theme());
        let ctx = PreviewCtx {
            width: 40,
            cache: None,
        };

        // When rendering the preview.
        let lines = endpoint_preview(&entry, &ctx);

        // Then it is the one-line explanation.
        assert_eq!(lines.len(), 1);
    }

    #[rstest::rstest]
    #[test]
    fn preview_shows_metadata_for_a_real_upstream() {
        // Given an upstream with uptime and prices.
        let entry = EndpointEntry {
            tag: "anthropic".to_owned(),
            provider_name: "Anthropic".to_owned(),
            uptime_30m: Some(99.2),
            prompt_price: Some("$3".to_owned()),
            completion_price: Some("$15".to_owned()),
            quantization: None,
            max_completion_tokens: Some(64_000),
            is_active: false,
            theme: default_theme(),
        };
        let ctx = PreviewCtx {
            width: 40,
            cache: None,
        };

        // When rendering the preview.
        let lines = endpoint_preview(&entry, &ctx);

        // Then all six metadata rows render.
        assert_eq!(lines.len(), 6);
    }

    // ── Render (status line) ──────────────────────────────────────────

    /// Renders the status hook against a read-only host over `state`.
    fn status_line_of(state: &AppState) -> Line<'static> {
        let host = crate::feat::picker::host_impl::AppStateRenderHost::new(state);
        let ctx = StatusCtx::new(
            PickerId::new(crate::feat::picker::registry::ENDPOINT_ID),
            &host,
        );
        endpoint_status(&ctx).expect("status line always renders")
    }

    #[rstest::rstest]
    #[test]
    fn status_shows_fetching_indicator_while_loading() {
        // Given a populated picker mid-fetch (loading flag set).
        let mut state = AppState::default();
        state
            .frontend
            .endpoint_picker_mut()
            .set_items(wrap(vec![EndpointEntry::auto_route(true, default_theme())]));
        state.frontend.pickers.endpoint_loading = true;

        // When rendering the status line.
        let line = status_line_of(&state);

        // Then it contains the fetching indicator.
        let text: String = line.spans.iter().map(|s| s.content.clone()).collect();
        assert!(
            text.contains("fetching"),
            "status must show a fetching indicator while loading: {text}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn status_shows_fetched_age_when_not_loading() {
        // Given a populated picker with a fetch timestamp and loading cleared.
        let mut state = AppState::default();
        state
            .frontend
            .endpoint_picker_mut()
            .set_items(wrap(vec![EndpointEntry::auto_route(true, default_theme())]));
        state.frontend.pickers.endpoint_fetched_at = Some(jiff::Timestamp::now());

        // When rendering the status line.
        let line = status_line_of(&state);

        // Then it contains a freshness line.
        let text: String = line.spans.iter().map(|s| s.content.clone()).collect();
        assert!(
            text.contains("fetched"),
            "status must show a freshness line when fetched_at is set: {text}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn status_shows_the_pinned_upstream() {
        // Given a picker whose active entry is Anthropic.
        let mut state = AppState::default();
        state
            .frontend
            .endpoint_picker_mut()
            .set_items(wrap(vec![EndpointEntry {
                tag: "anthropic".to_owned(),
                provider_name: "Anthropic".to_owned(),
                uptime_30m: None,
                prompt_price: None,
                completion_price: None,
                quantization: None,
                max_completion_tokens: None,
                is_active: true,
                theme: default_theme(),
            }]));

        // When rendering the status line.
        let line = status_line_of(&state);

        // Then it shows the routing target.
        let text: String = line.spans.iter().map(|s| s.content.clone()).collect();
        assert!(
            text.contains("Routing: Anthropic"),
            "status must show the pinned upstream: {text}"
        );
    }
}
