//! The plugin picker's spec — behavior authored once in the builder.
//!
//! A read-only roster: one row per known plugin (name + latest lifecycle
//! phase) snapshotted from the plugin contribution cache at open time.
//! Enter is a no-op — plugins are managed through jinn.toml and lifecycle
//! events, not from the picker.

use jinn_picker::ActionCtx;
use jinn_picker::PickerId;
use jinn_picker::PickerOutcome;
use jinn_picker::PickerSpec;
use jinn_picker::RowCtx;
use ratatui::text::Line;
use ratatui::text::Span;

use crate::common::app_state::AppState;
use crate::feat::plugin::PluginPickerEntry;
use crate::feat::plugin_coordinator_actor::protocol::PluginPhase;
use crate::feat::ui::picker_states::PickerExt;

/// Builds the plugin picker's spec.
#[must_use]
pub fn plugin_spec() -> PickerSpec<PluginPickerEntry> {
    PickerSpec::new(PickerId::new(crate::feat::picker::registry::PLUGIN_ID))
        .title(" Plugins ")
        .row(plugin_row)
        .search(|entry| entry.name.clone())
        .on_open(open_plugins)
}

/// The domain state behind an [`ActionCtx`]. The kernel's host lens always
/// lends `AppState`; this downcast is the spec's single sanctioned escape.
fn state_of<'a>(ctx: &'a mut ActionCtx<'_>) -> &'a mut AppState {
    ctx.state_any()
        .downcast_mut::<AppState>()
        .expect("domain host lends AppState")
}

// ── Rendering ────────────────────────────────────────────────────────────

/// The lifecycle phase's alarm color: live phases accent, dead phases
/// error, and a clean exit stays neutral.
fn phase_color(phase: PluginPhase, theme: &crate::feat::theme::Theme) -> ratatui::style::Color {
    match phase {
        PluginPhase::Starting | PluginPhase::Running => theme.focus_accent,
        PluginPhase::Dead | PluginPhase::Unresponsive => theme.error_text,
        // A clean, run-to-completion exit: neutral, not an alarm color.
        PluginPhase::Done => theme.muted_text,
    }
}

/// Renders one picker row: the plugin name, a raw `·` separator, and the
/// phase label in its lifecycle color.
pub fn plugin_row(entry: &PluginPickerEntry, ctx: &RowCtx<'_>) -> Line<'static> {
    let name_style = if ctx.is_selected {
        ratatui::style::Style::default()
            .fg(entry.theme.primary_text)
            .bg(entry.theme.picker_selected_bg)
    } else {
        ratatui::style::Style::default()
    };

    let phase_style = if ctx.is_selected {
        ratatui::style::Style::default().bg(entry.theme.picker_selected_bg)
    } else {
        ratatui::style::Style::default()
    };

    Line::from(vec![
        Span::styled(entry.name.clone(), name_style),
        Span::raw(" \u{b7} ".to_owned()),
        Span::styled(
            format!("{:?}", entry.phase),
            phase_style.fg(phase_color(entry.phase, &entry.theme)),
        ),
    ])
}

// ── Lifecycle ────────────────────────────────────────────────────────────

/// Opening the plugin picker: fresh filter + selection, then snapshot one
/// read-only entry per cached plugin in name order. Opening never touches
/// the plugin coordinator — it reads whatever phases were last mirrored
/// into the cache.
fn open_plugins(ctx: &mut ActionCtx<'_>) -> PickerOutcome {
    let state = state_of(ctx);
    state.frontend.plugin_picker_mut().reset();

    let entries: Vec<PluginPickerEntry> = state
        .plugins
        .phases()
        .map(|(name, phase)| {
            PluginPickerEntry::new(name.to_owned(), phase, state.frontend.theme.clone())
        })
        .collect();

    let wrapped = {
        let registry = crate::feat::picker::registry::build_picker_registry();
        registry
            .make_items(crate::feat::picker::registry::PLUGIN_ID, entries)
            .unwrap_or_default()
    };
    state.frontend.plugin_picker_mut().set_items(wrapped);
    PickerOutcome::empty()
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        reason = "test module, panics are acceptable"
    )]
    use super::*;
    use crate::common::app_state::AppState;
    use crate::feat::picker::PickerKind;
    use crate::feat::picker::intent::handle_open_picker;

    /// State with the given plugins mirrored into the contribution cache.
    fn state_with(phases: &[(&str, PluginPhase)]) -> AppState {
        let mut state = AppState::default_with_scope_focus();
        for (name, phase) in phases {
            state.plugins.set_phase((*name).to_owned(), *phase);
        }
        state
    }

    #[rstest::rstest]
    fn open_loads_one_entry_per_cached_phase_in_name_order() {
        // Given plugins mirrored into the cache out of alphabetical order.
        let mut state = state_with(&[("zeta", PluginPhase::Running), ("alpha", PluginPhase::Dead)]);

        // When opening the plugin picker through the real open path.
        let registry = crate::feat::picker::registry::build_picker_registry();
        handle_open_picker(&mut state, PickerKind::Plugin, &registry);

        // Then the picker holds one wrapped entry per plugin, name-ordered.
        let names: Vec<&str> = state
            .frontend
            .plugin_picker()
            .items()
            .iter()
            .map(|item| item.entry().name.as_str())
            .collect();
        assert_eq!(names, vec!["alpha", "zeta"]);
        // And each entry carries its phase.
        assert_eq!(
            state.frontend.plugin_picker().items()[1].entry().phase,
            PluginPhase::Running
        );
    }

    #[rstest::rstest]
    fn confirm_is_a_noop_and_keeps_the_picker_open() {
        // Given a plugin picker open with one entry.
        let mut state = state_with(&[("plugin-x", PluginPhase::Running)]);
        let registry = crate::feat::picker::registry::build_picker_registry();
        handle_open_picker(&mut state, PickerKind::Plugin, &registry);

        // When confirming through the real confirm path.
        let (result, _redispatch) =
            crate::feat::picker::intent::handle_picker_confirm(&mut state, &registry);

        // Then nothing is emitted and the picker stays open (read-only).
        assert!(result.message_names.is_empty());
        assert_eq!(state.frontend.picker_kind(), Some(PickerKind::Plugin));
    }

    #[rstest::rstest]
    fn open_with_empty_cache_opens_empty() {
        // Given no plugins in the contribution cache.
        let mut state = AppState::default_with_scope_focus();

        // When opening the plugin picker through the real open path.
        let registry = crate::feat::picker::registry::build_picker_registry();
        handle_open_picker(&mut state, PickerKind::Plugin, &registry);

        // Then the picker holds zero entries.
        assert!(state.frontend.plugin_picker().items().is_empty());
    }

    #[rstest::rstest]
    #[case::starting(PluginPhase::Starting, "Starting")]
    #[case::running(PluginPhase::Running, "Running")]
    #[case::dead(PluginPhase::Dead, "Dead")]
    #[case::unresponsive(PluginPhase::Unresponsive, "Unresponsive")]
    #[case::done(PluginPhase::Done, "Done")]
    fn row_shows_name_and_phase_label(#[case] phase: PluginPhase, #[case] label: &str) {
        // Given a plugin entry with this phase.
        let entry = PluginPickerEntry::new(
            "plugin-x".to_owned(),
            phase,
            crate::feat::theme::default_theme(),
        );

        // When rendering the row.
        let line = plugin_row(&entry, &RowCtx::flat(false, &[]));
        let text: String = line.spans.iter().map(|s| s.content.to_string()).collect();

        // Then the name and phase label both appear.
        assert!(text.contains("plugin-x"), "row shows name: {text}");
        assert!(text.contains(label), "row shows phase {label}: {text}");
    }
}
