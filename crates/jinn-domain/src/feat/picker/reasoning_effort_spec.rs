//! The reasoning-effort picker's spec — behavior authored once in the builder.
//!
//! Open builds the seven effort entries inline from the session's own
//! reasoning override (no actor round-trip), Enter writes the chosen effort
//! back to the session profile and seeds the global default, and the status
//! row shows the live `Active: <name>` marker.

use jinn_picker::ActionCtx;
use jinn_picker::PickerId;
use jinn_picker::PickerOutcome;
use jinn_picker::PickerSpec;
use jinn_picker::RowCtx;
use jinn_picker::StatusCtx;
use jinn_selection_widget::highlight_text_with_bg;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::text::Span;

use crate::common::app_state::AppState;
use crate::feat::picker::style::dim_style;
use crate::feat::reasoning::ReasoningEffort;
use crate::feat::reasoning::ReasoningEffortEntry;
use crate::feat::reasoning::resolve_effort;
use crate::feat::ui::picker_states::PickerExt;

/// Builds the reasoning-effort picker's spec.
#[must_use]
pub fn reasoning_effort_spec() -> PickerSpec<ReasoningEffortEntry> {
    PickerSpec::new(PickerId::new(
        crate::feat::picker::registry::REASONING_EFFORT_ID,
    ))
    .title(" Reasoning Effort ")
    .row(reasoning_row)
    .search(|entry| format!("{} {}", entry.name, entry.description))
    .on_open(open_reasoning)
    .on_confirm(confirm_reasoning)
    .status(reasoning_status)
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

/// Human-readable description for each effort variant.
///
/// Display-only; the wire value is [`ReasoningEffort::as_str`].
fn effort_description(effort: ReasoningEffort) -> &'static str {
    match effort {
        ReasoningEffort::Max => "Maximum effort",
        ReasoningEffort::Xhigh => "Extra-high effort",
        ReasoningEffort::High => "High effort",
        ReasoningEffort::Medium => "Medium effort",
        ReasoningEffort::Low => "Low effort",
        ReasoningEffort::Minimal => "Minimal effort",
        ReasoningEffort::None => "Skip reasoning",
    }
}

/// All seven effort variants in declaration order.
///
/// Kept in sync with the `ReasoningEffort` enum. Serves as the single source of
/// truth for the picker's row order.
const ALL_EFFORTS: [ReasoningEffort; 7] = [
    ReasoningEffort::Max,
    ReasoningEffort::Xhigh,
    ReasoningEffort::High,
    ReasoningEffort::Medium,
    ReasoningEffort::Low,
    ReasoningEffort::Minimal,
    ReasoningEffort::None,
];

// ── Rendering ────────────────────────────────────────────────────────────

/// Renders one picker row: the bold `> ` marker on the active effort, the
/// effort name (two trailing spaces), and the description dimmed. Filter
/// matches highlight within the name.
pub fn reasoning_row(entry: &ReasoningEffortEntry, ctx: &RowCtx<'_>) -> Line<'static> {
    let active_marker = Span::styled(
        if entry.is_active { "> " } else { "  " },
        if entry.is_active {
            Style::default()
                .fg(entry.theme.picker_active_marker)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        },
    );

    let name_style = if ctx.is_selected {
        Style::default()
            .fg(entry.theme.primary_text)
            .bg(entry.theme.picker_selected_bg)
    } else {
        Style::default()
    };

    let desc_style = dim_style(ctx.is_selected, &entry.theme);

    let name_spans = if ctx.match_ranges.is_empty() {
        vec![Span::styled(format!("{}  ", entry.name), name_style)]
    } else {
        let mut spans = highlight_text_with_bg(
            &entry.name,
            name_style,
            ctx.match_ranges,
            entry.theme.picker_highlight_bg,
        );
        spans.push(Span::styled("  ".to_owned(), name_style));
        spans
    };

    let mut all_spans = vec![active_marker];
    all_spans.extend(name_spans);
    all_spans.push(Span::styled(entry.description.clone(), desc_style));
    Line::from(all_spans)
}

/// The status line: the active effort name, e.g. `Active: high`. Reads the
/// pre-computed `is_active` flag from the loaded entries rather than
/// re-resolving the effort (AppState has no preferences access here).
#[expect(clippy::unnecessary_wraps, reason = "hook signature is Option<Line>")]
fn reasoning_status(ctx: &StatusCtx<'_>) -> Option<Line<'static>> {
    let state = state_ref_of(ctx);
    let gray = Style::default().fg(state.frontend.theme.muted_text);
    let active_name = state
        .frontend
        .reasoning_effort_picker()
        .items()
        .iter()
        .find(|item| item.entry().is_active)
        .map_or("none", |item| item.entry().name.as_str());
    Some(Line::from(vec![
        Span::styled("Active: ".to_owned(), gray),
        Span::styled(
            active_name.to_owned(),
            Style::default().fg(state.frontend.theme.primary_text),
        ),
    ]))
}

// ── Lifecycle ────────────────────────────────────────────────────────────

/// Opening the effort picker: fresh filter + selection, then build the seven
/// entries inline — one per variant, marking the session's own resolved
/// effort active. Opening never touches the actor system.
fn open_reasoning(ctx: &mut ActionCtx<'_>) -> PickerOutcome {
    let state = state_of(ctx);
    state.frontend.reasoning_effort_picker_mut().reset();

    // The session owns its effort (seeded from the global at creation).
    let active = resolve_effort(state.active_session().profile().reasoning_effort);
    let theme = state.frontend.theme.clone();

    let entries = ALL_EFFORTS
        .iter()
        .map(|&effort| ReasoningEffortEntry {
            effort,
            name: effort.as_str().to_owned(),
            description: effort_description(effort).to_owned(),
            is_active: active == Some(effort),
            theme: theme.clone(),
        })
        .collect::<Vec<_>>();

    let wrapped = {
        let registry = crate::feat::picker::registry::build_picker_registry();
        registry
            .make_items(crate::feat::picker::registry::REASONING_EFFORT_ID, entries)
            .unwrap_or_default()
    };
    state
        .frontend
        .reasoning_effort_picker_mut()
        .set_items(wrapped);
    PickerOutcome::empty()
}

/// Enter on the effort picker: write the session's reasoning override and
/// seed the global default, then close. The session emit persists the
/// session immediately; the app-state update seeds future sessions.
fn confirm_reasoning(ctx: &mut ActionCtx<'_>) -> PickerOutcome {
    let Some(effort) = state_of(ctx)
        .frontend
        .reasoning_effort_picker()
        .selected_item()
        .map(|item| item.entry().effort)
    else {
        return PickerOutcome::empty();
    };
    let session_id = {
        let state = state_of(ctx);
        state.active_session_mut().profile_mut().reasoning_effort = Some(effort);
        state.session.active_session_id().clone()
    };

    PickerOutcome::empty()
        .with_message(crate::feat::session::protocol::mark_session_interacted::MarkSessionInteracted {
            session_id,
        })
        .with_message(
            crate::feat::preferences_actor::protocol::app_state_command::UpdateAppState {
                updates: vec![
                    crate::feat::preferences_actor::protocol::app_state_command::AppStateUpdate::SetReasoningEffort(
                        Some(effort),
                    ),
                ],
            },
        )
        .close()
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test module, panics are acceptable"
    )]
    use super::*;
    use crate::common::app_state::FocusScope;
    use crate::feat::picker::PickerKind;
    use crate::feat::picker::host_impl::AppStatePickerHost;
    use crate::feat::picker::registry::REASONING_EFFORT_ID;
    use crate::feat::session::chat_session::ChatSessionState;

    /// State with an active origin session (the default map's session).
    fn state_with_session() -> AppState {
        let mut state = AppState::default_with_scope_focus();
        state
            .session
            .set_active(state.session.active_session_id().clone());
        state
    }

    /// Opens the picker through the real open path (scope push + spec open
    /// hook), mirroring what the intent handler does.
    fn open(state: &mut AppState) {
        let registry = crate::feat::picker::registry::build_picker_registry();
        crate::feat::picker::intent::handle_open_picker(
            state,
            PickerKind::ReasoningEffort,
            &registry,
        );
    }

    /// Runs a spec hook against `state` with a fresh dispatch context.
    fn run(
        state: &mut AppState,
        f: impl FnOnce(&mut ActionCtx<'_>) -> PickerOutcome,
    ) -> PickerOutcome {
        let mut host = AppStatePickerHost::new(state);
        let mut ctx = ActionCtx::new(PickerId::new(REASONING_EFFORT_ID), &mut host);
        f(&mut ctx)
    }

    /// Selects the entry whose effort matches.
    fn select(state: &mut AppState, effort: ReasoningEffort) {
        let index = state
            .frontend
            .reasoning_effort_picker()
            .items()
            .iter()
            .position(|item| item.entry().effort == effort)
            .unwrap_or_else(|| panic!("entry for {effort:?}"));
        for _ in 0..index {
            state.frontend.reasoning_effort_picker_mut().move_down(1);
        }
    }

    // ── Open ─────────────────────────────────────────────────────────

    #[rstest::rstest]
    #[test]
    fn open_builds_seven_effort_entries_inline() {
        // Given a plain active session and no pending load messages.
        let mut state = state_with_session();

        // When opening the picker.
        open(&mut state);

        // Then all seven efforts load in declaration order with names and
        // descriptions — synchronously, with no actor round-trip message.
        let items = state.frontend.reasoning_effort_picker().items();
        assert_eq!(items.len(), 7, "one entry per variant");
        let names: Vec<&str> = items.iter().map(|i| i.entry().name.as_str()).collect();
        assert_eq!(
            names,
            vec!["max", "xhigh", "high", "medium", "low", "minimal", "none"]
        );
        assert!(
            items.iter().all(|i| !i.entry().description.is_empty()),
            "every entry carries a description"
        );
    }

    #[rstest::rstest]
    #[test]
    fn open_marks_the_resolved_session_effort_active() {
        // Given a session whose own effort override is Low.
        let mut state = state_with_session();
        state.active_session_mut().profile_mut().reasoning_effort = Some(ReasoningEffort::Low);

        // When opening the picker.
        open(&mut state);

        // Then exactly the Low entry is active.
        let items = state.frontend.reasoning_effort_picker().items();
        let active: Vec<&str> = items
            .iter()
            .filter(|i| i.entry().is_active)
            .map(|i| i.entry().name.as_str())
            .collect();
        assert_eq!(active, vec!["low"]);
    }

    #[rstest::rstest]
    #[test]
    fn open_marks_no_entry_active_when_session_unset() {
        // Given a session with no effort override (the global default is
        // irrelevant to the picker by design).
        let mut state = state_with_session();

        // When opening the picker.
        open(&mut state);

        // Then no entry is active.
        let active = state
            .frontend
            .reasoning_effort_picker()
            .items()
            .iter()
            .filter(|i| i.entry().is_active)
            .count();
        assert_eq!(active, 0, "global default must not mark any entry active");
    }

    // ── Confirm ──────────────────────────────────────────────────────

    #[rstest::rstest]
    #[test]
    fn confirm_on_empty_picker_is_a_no_op() {
        // Given an open picker whose entries were never populated.
        let mut state = state_with_session();
        state.frontend.scope_push(FocusScope::Picker {
            kind: PickerKind::ReasoningEffort,
        });

        // When running the confirm hook.
        let outcome = run(&mut state, confirm_reasoning);

        // Then nothing happened: no write, no messages, no close.
        assert_eq!(
            state.active_session().profile().reasoning_effort,
            None,
            "no override was written"
        );
        assert!(outcome.messages.is_empty());
        assert!(!outcome.close);
    }

    #[rstest::rstest]
    #[test]
    fn confirm_sets_session_override_and_emits_both_messages() {
        // Given an open picker with Xhigh selected.
        let mut state = state_with_session();
        open(&mut state);
        select(&mut state, ReasoningEffort::Xhigh);

        // When confirming.
        let outcome = run(&mut state, confirm_reasoning);

        // Then the session's own override is Xhigh.
        assert_eq!(
            state.active_session().profile().reasoning_effort,
            Some(ReasoningEffort::Xhigh),
        );
        // And both persistence messages were emitted.
        assert!(
            outcome
                .message_names
                .iter()
                .any(|n| n.ends_with("MarkSessionInteracted")),
            "MarkSessionInteracted must be emitted: {:?}",
            outcome.message_names
        );
        assert!(
            outcome
                .message_names
                .iter()
                .any(|n| n.ends_with("UpdateAppState")),
            "UpdateAppState (global seed) must be emitted: {:?}",
            outcome.message_names
        );
        // And the picker closes.
        assert!(outcome.close);
    }

    #[rstest::rstest]
    #[test]
    fn confirm_in_session_a_does_not_leak_into_session_b() {
        // Regression: each session owns its effort; the global seed message
        // must not retroactively change another session's resolved value.
        let mut state = state_with_session();

        // Session B: seeded with High (its own frozen value).
        let mut b = ChatSessionState::new();
        b.profile_mut().reasoning_effort = Some(ReasoningEffort::High);
        let b_id = b.session_id().clone();
        state.session.insert(b);

        // Session A (active): change to Xhigh via the picker.
        open(&mut state);
        select(&mut state, ReasoningEffort::Xhigh);

        // When confirming in session A.
        let _outcome = run(&mut state, confirm_reasoning);

        // Then session A holds Xhigh while session B still resolves High.
        assert_eq!(
            state.active_session().profile().reasoning_effort,
            Some(ReasoningEffort::Xhigh),
        );
        assert_eq!(
            resolve_effort(state.session.get(&b_id).unwrap().profile().reasoning_effort),
            Some(ReasoningEffort::High),
            "session B's own effort must be unaffected"
        );
    }

    #[rstest::rstest]
    #[test]
    fn confirm_through_dispatch_clears_the_picker_scope() {
        // Given an open picker (scope pushed) with a selection.
        let mut state = state_with_session();
        open(&mut state);
        select(&mut state, ReasoningEffort::Medium);

        // When confirming through the real confirm path (hook + fold).
        let _ = crate::feat::picker::intent::handle_picker_confirm(
            &mut state,
            &crate::feat::picker::registry::build_picker_registry(),
        );

        // Then the scope stack has no ReasoningEffort picker left (fold
        // cleared overlays; nothing else was open).
        let still_open = state
            .frontend
            .picker_kind()
            .is_some_and(|k| k == PickerKind::ReasoningEffort);
        assert!(!still_open, "picker scope should be gone after confirm");
    }

    // ── Status ───────────────────────────────────────────────────────

    #[rstest::rstest]
    #[test]
    fn status_row_shows_active_effort() {
        // Given a session with a Low override and a loaded picker.
        let mut state = state_with_session();
        state.active_session_mut().profile_mut().reasoning_effort = Some(ReasoningEffort::Low);
        open(&mut state);

        // When rendering the status hook.
        let host = AppStatePickerHost::new(&mut state);
        let ctx = StatusCtx::new(PickerId::new(REASONING_EFFORT_ID), &host);
        let line = reasoning_status(&ctx).expect("status is Some");

        // Then it reads "Active: low".
        assert_eq!(line.to_string(), "Active: low");
    }

    #[rstest::rstest]
    #[test]
    fn status_row_shows_none_when_no_entry_active() {
        // Given a session without an override and a loaded picker.
        let mut state = state_with_session();
        open(&mut state);

        // When rendering the status hook.
        let host = AppStatePickerHost::new(&mut state);
        let ctx = StatusCtx::new(PickerId::new(REASONING_EFFORT_ID), &host);
        let line = reasoning_status(&ctx).expect("status is Some");

        // Then it reads "Active: none".
        assert_eq!(line.to_string(), "Active: none");
    }

    // ── Rows ─────────────────────────────────────────────────────────

    #[rstest::rstest]
    #[test]
    fn row_renders_bold_arrow_marker_on_active_entry() {
        // Given an active entry.
        let entry = ReasoningEffortEntry {
            effort: ReasoningEffort::High,
            name: "high".to_owned(),
            description: "High effort".to_owned(),
            is_active: true,
            theme: crate::feat::theme::default_theme(),
        };
        let ranges = Vec::new();
        let ctx = RowCtx::flat(false, &ranges);

        // When rendering the row.
        let line = reasoning_row(&entry, &ctx);

        // Then it leads with a bold arrow.
        assert!(line.to_string().starts_with("> high"));
        assert!(line.spans[0].style.add_modifier.contains(Modifier::BOLD));
    }

    #[rstest::rstest]
    #[test]
    fn row_renders_blank_marker_and_description_on_inactive_entry() {
        // Given an inactive, unselected entry.
        let entry = ReasoningEffortEntry {
            effort: ReasoningEffort::Low,
            name: "low".to_owned(),
            description: "Low effort".to_owned(),
            is_active: false,
            theme: crate::feat::theme::default_theme(),
        };
        let ranges = Vec::new();
        let ctx = RowCtx::flat(false, &ranges);

        // When rendering the row.
        let line = reasoning_row(&entry, &ctx);

        // Then it leads with two blanks and carries the description.
        let text = line.to_string();
        assert!(text.starts_with("  low"), "got: {text}");
        assert!(text.contains("Low effort"));
    }

    #[rstest::rstest]
    #[test]
    fn row_highlights_matched_name_with_background() {
        // Given an entry whose name matches the filter at 0..2.
        let entry = ReasoningEffortEntry {
            effort: ReasoningEffort::High,
            name: "high".to_owned(),
            description: "High effort".to_owned(),
            is_active: false,
            theme: crate::feat::theme::default_theme(),
        };
        let ranges = vec![0..2];
        let ctx = RowCtx::flat(false, &ranges);

        // When rendering the row.
        let line = reasoning_row(&entry, &ctx);

        // Then the matched span carries the highlight background.
        let highlighted = &line.spans[1];
        assert_eq!(highlighted.content.as_ref(), "hi");
        assert!(
            highlighted.style.bg.is_some(),
            "match span carries highlight bg"
        );
    }
}
