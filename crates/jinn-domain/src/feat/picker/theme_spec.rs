//! The theme picker's spec — behavior authored once in the builder.
//!
//! The theme picker live-previews: moving the cursor applies the
//! highlighted theme immediately (invalidating theme caches per move), ESC
//! restores the snapshotted pre-open theme, and Enter persists the
//! already-applied theme via `UpdateAppState::SetTheme`. Open resets the
//! picker, snapshots the current theme, and loads entries from the plugin
//! contribution cache — the built-in "default" pinned first (a contributed
//! "default" replaces the built-in entry's look while keeping its reserved
//! slot), the rest sorted case-insensitively by name.

use jinn_picker::ActionCtx;
use jinn_picker::PickerId;
use jinn_picker::PickerOutcome;
use jinn_picker::PickerSpec;
use jinn_picker::RowCtx;
use jinn_picker::StatusCtx;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::text::Span;

use crate::common::app_state::AppState;
use crate::feat::preferences_actor::protocol::app_state_command::AppStateUpdate;
use crate::feat::preferences_actor::protocol::app_state_command::UpdateAppState;
use crate::feat::theme::ThemeEntry;
use crate::feat::ui::picker_states::PickerExt;

/// Builds the theme picker's spec.
#[must_use]
pub fn theme_spec() -> PickerSpec<ThemeEntry> {
    PickerSpec::new(PickerId::new(crate::feat::picker::registry::THEME_ID))
        .title(" Themes ")
        .row(theme_row)
        .search(|entry| entry.name.clone())
        .on_open(open_theme)
        .on_selection_change(preview_theme)
        .on_confirm(confirm_theme)
        .on_close(restore_theme)
        .status(theme_status)
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

/// Renders one picker row: the theme's focus-accent swatch followed by the
/// name (selected rows carry the selection background). Filter matches are
/// not highlighted — identical to the legacy theme row rendering.
fn theme_row(entry: &ThemeEntry, ctx: &RowCtx<'_>) -> Line<'static> {
    let style = if ctx.is_selected {
        Style::default()
            .fg(entry.theme.primary_text)
            .bg(entry.theme.picker_selected_bg)
    } else {
        Style::default()
    };

    let swatch = Span::styled(
        "\u{2588} ".to_owned(), // █
        Style::default().fg(entry.theme.focus_accent),
    );
    let name = Span::styled(entry.name.clone(), style);
    Line::from(vec![swatch, name])
}

/// The status line: the persisted theme name (what ESC restores to and what
/// stays after restart), "default" when none is persisted.
#[expect(clippy::unnecessary_wraps, reason = "hook signature is Option<Line>")]
fn theme_status(ctx: &StatusCtx<'_>) -> Option<Line<'static>> {
    let state = state_ref_of(ctx);
    let current = state
        .frontend
        .app_state
        .theme_name
        .as_deref()
        .unwrap_or("default");
    Some(Line::from(vec![
        Span::styled(
            "Current: ".to_owned(),
            Style::default().fg(state.frontend.theme.muted_text),
        ),
        Span::styled(
            current.to_owned(),
            Style::default().fg(state.frontend.theme.primary_text),
        ),
    ]))
}

// ── Lifecycle ────────────────────────────────────────────────────────────

/// Opening the theme picker: fresh filter + selection, snapshot the current
/// theme for the ESC revert, and load entries from the contribution cache.
fn open_theme(ctx: &mut ActionCtx<'_>) -> PickerOutcome {
    let state = state_of(ctx);
    state.frontend.theme_picker_mut().reset();
    // Save current theme so ESC can restore it.
    *state.frontend.theme_preview_original_mut() = Some(state.frontend.theme.clone());
    load_theme_picker_entries(state);
    PickerOutcome::empty()
}

/// Selection change (cursor movement, paging): apply the highlighted theme
/// live. Cache invalidation keeps every theme-sensitive view consistent with
/// the preview.
fn preview_theme(entry: &ThemeEntry, ctx: &mut ActionCtx<'_>) {
    let state = state_of(ctx);
    state.frontend.theme = entry.theme.clone();
    state.invalidate_theme_caches();
}

/// Enter on the theme picker: the highlighted theme is already applied (set
/// on move) — clear the snapshot and persist the name. With no selection
/// there is nothing to persist and the picker stays open.
fn confirm_theme(ctx: &mut ActionCtx<'_>) -> PickerOutcome {
    let state = state_of(ctx);
    let Some(entry) = state.frontend.theme_picker().selected_item() else {
        return PickerOutcome::empty();
    };
    let theme_name = entry.entry().name.clone();

    *state.frontend.theme_preview_original_mut() = None;
    PickerOutcome::new_message(UpdateAppState {
        updates: vec![AppStateUpdate::SetTheme(Some(theme_name))],
    })
    .close()
}

/// ESC on the theme picker (the revert path — never the confirm path):
/// restore the snapshotted pre-open theme. Signals `close` so the dispatch
/// layer pops the picker scope.
fn restore_theme(ctx: &mut ActionCtx<'_>) -> PickerOutcome {
    let state = state_of(ctx);
    if let Some(original) = state.frontend.theme_preview_original_mut().take() {
        state.frontend.theme = original;
        state.invalidate_theme_caches();
    }
    PickerOutcome::empty().close()
}

/// Loads themes into the theme picker from the plugin contribution cache.
///
/// The built-in default always leads; contributed themes follow in name
/// order. Opening the picker never touches the filesystem — it reads
/// whatever the themes plugin last pushed (a dead plugin means default only).
fn load_theme_picker_entries(state: &mut AppState) {
    let entries = {
        let mut entries = vec![ThemeEntry {
            name: "default".to_owned(),
            theme: crate::feat::theme::default_theme(),
        }];

        // A user-contributed "default" replaces the built-in entry's look
        // while keeping its reserved slot.
        if let (Some(entry), Some(contributed)) =
            (entries.first_mut(), state.plugins.theme("default"))
        {
            entry.theme = contributed.theme.clone();
        }

        for (name, contributed) in state.plugins.themes() {
            if name == "default" {
                continue;
            }
            entries.push(ThemeEntry {
                name: name.to_owned(),
                theme: contributed.theme.clone(),
            });
        }

        // Default stays pinned first; the rest follow in case-insensitive
        // name order.
        let mut rest = entries.split_off(1);
        rest.sort_by_key(|e| e.name.to_lowercase());
        entries.extend(rest);
        entries
    };

    let wrapped = {
        let registry = crate::feat::picker::registry::build_picker_registry();
        registry
            .make_items(crate::feat::picker::registry::THEME_ID, entries)
            .unwrap_or_default()
    };
    state.frontend.theme_picker_mut().set_items(wrapped);
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic,
        reason = "test code"
    )]
    use super::*;
    use jinn_selection_widget::PreviewCache as _;

    fn state_with_themes(contributed: &[(&str, crate::feat::theme::Theme)]) -> AppState {
        let mut state = AppState::default();
        state.plugins.set_themes(
            "theme-loader",
            contributed
                .iter()
                .map(|(name, theme)| ((*name).to_owned(), None, theme.clone()))
                .collect(),
        );
        state
    }

    fn open(state: &mut AppState) {
        let registry = crate::feat::picker::registry::build_picker_registry();
        let picker_id = PickerId::new(crate::feat::picker::registry::THEME_ID);
        let mut host = crate::feat::picker::host_impl::AppStatePickerHost::new(state);
        let mut ctx = ActionCtx::new(picker_id, &mut host);
        let spec = registry
            .get(crate::feat::picker::registry::THEME_ID)
            .expect("theme spec registered");
        spec.run_open(&mut ctx);
    }

    #[rstest::rstest]
    #[test]
    fn open_resets_snapshots_and_loads_default_first_then_contributed_sorted() {
        // Given a cache with unsorted contributed themes and a non-default
        // active theme.
        let mut state = state_with_themes(&[
            ("zeta", crate::feat::theme::default_theme()),
            ("Beta", crate::feat::theme::default_theme()),
            ("alpha", crate::feat::theme::default_theme()),
        ]);
        state.frontend.theme = crate::feat::theme::default_theme();

        // When opening the theme picker.
        open(&mut state);

        // Then default leads and the rest follow case-insensitively sorted.
        let names: Vec<&str> = state
            .frontend
            .theme_picker()
            .items()
            .iter()
            .map(|item| item.entry().name.as_str())
            .collect();
        assert_eq!(names, vec!["default", "alpha", "Beta", "zeta"]);
        // And the pre-open theme is snapshotted for the ESC revert.
        assert!(
            state.frontend.theme_preview_original().is_some(),
            "open must snapshot the current theme"
        );
    }

    #[rstest::rstest]
    #[test]
    fn open_with_contributed_default_keeps_reserved_slot_but_borrows_its_look() {
        // Given a contributed theme named "default" (distinct from the
        // built-in default theme).
        let contributed = crate::feat::theme::default_theme();
        let mut state = state_with_themes(&[("default", contributed.clone())]);

        // When opening the theme picker.
        open(&mut state);

        // Then the reserved first slot is kept and carries the
        // contributed theme's look.
        let items = state.frontend.theme_picker().items();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].entry().name, "default");
        assert_eq!(
            items[0].entry().theme.focus_accent,
            contributed.focus_accent
        );
    }

    #[rstest::rstest]
    #[test]
    fn open_with_empty_cache_shows_default_only() {
        // Given no plugin contributions (dead or absent themes plugin).
        let mut state = AppState::default();

        // When opening the theme picker.
        open(&mut state);

        // Then the picker offers exactly the built-in default.
        let items = state.frontend.theme_picker().items();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].entry().name, "default");
    }

    #[rstest::rstest]
    #[test]
    fn selection_change_previews_the_highlighted_theme_and_invalidates_caches() {
        // Given an open picker whose second entry is a distinct theme, with
        // a populated theme-sensitive cache.
        let mut other = crate::feat::theme::default_theme();
        other.focus_accent = ratatui::style::Color::Red;
        let mut state = state_with_themes(&[("other", other)]);
        open(&mut state);
        state
            .frontend
            .caches
            .skill_preview_cache
            .insert("12345".to_owned(), 80, Vec::new());
        assert_eq!(state.frontend.caches.skill_preview_cache.len(), 1);
        let registry = crate::feat::picker::registry::build_picker_registry();
        let picker_id = PickerId::new(crate::feat::picker::registry::THEME_ID);
        let spec = registry
            .get(crate::feat::picker::registry::THEME_ID)
            .expect("theme spec registered");

        // When the selection moves to the second entry.
        state.frontend.theme_picker_mut().move_down(10);
        {
            let mut host = crate::feat::picker::host_impl::AppStatePickerHost::new(&mut state);
            let mut ctx = ActionCtx::new(picker_id, &mut host);
            spec.run_selection_change(1, &mut ctx);
        }

        // Then the highlighted theme is applied live.
        assert_eq!(
            state.frontend.theme.focus_accent,
            ratatui::style::Color::Red
        );
        // And the theme-sensitive caches were invalidated.
        assert_eq!(
            state.frontend.caches.skill_preview_cache.len(),
            0,
            "selection change must invalidate theme caches"
        );
    }

    #[rstest::rstest]
    #[test]
    fn confirm_persists_set_theme_and_closes() {
        // Given an open picker with entries.
        let mut state = state_with_themes(&[("gruvbox", crate::feat::theme::default_theme())]);
        open(&mut state);
        let registry = crate::feat::picker::registry::build_picker_registry();
        let picker_id = PickerId::new(crate::feat::picker::registry::THEME_ID);

        // When confirming the selected theme.
        let outcome = {
            let mut host = crate::feat::picker::host_impl::AppStatePickerHost::new(&mut state);
            let mut ctx = ActionCtx::new(picker_id, &mut host);
            let spec = registry
                .get(crate::feat::picker::registry::THEME_ID)
                .expect("theme spec registered");
            spec.run_confirm(&mut ctx)
        };

        // Then SetTheme is emitted for the selected name.
        assert!(
            outcome
                .message_names
                .iter()
                .any(|n| n.contains("UpdateAppState")),
            "confirm must emit UpdateAppState::SetTheme; got {:?}",
            outcome.message_names
        );
        // And the snapshot is cleared so the revert cannot undo the choice.
        assert!(state.frontend.theme_preview_original().is_none());
        // And the picker closes.
        assert!(outcome.close);
    }

    #[rstest::rstest]
    #[test]
    fn confirm_with_no_selection_does_not_close() {
        // Given an open picker with no items (nothing selected).
        let mut state = AppState::default();
        open(&mut state);
        state.frontend.theme_picker_mut().set_items(Vec::new());
        let registry = crate::feat::picker::registry::build_picker_registry();
        let picker_id = PickerId::new(crate::feat::picker::registry::THEME_ID);

        // When confirming.
        let outcome = {
            let mut host = crate::feat::picker::host_impl::AppStatePickerHost::new(&mut state);
            let mut ctx = ActionCtx::new(picker_id, &mut host);
            let spec = registry
                .get(crate::feat::picker::registry::THEME_ID)
                .expect("theme spec registered");
            spec.run_confirm(&mut ctx)
        };

        // Then nothing is emitted and the picker stays open.
        assert!(outcome.message_names.is_empty());
        assert!(!outcome.close);
    }

    #[rstest::rstest]
    #[test]
    fn close_restores_the_snapshotted_theme_and_invalidates_caches() {
        // Given an open picker that previewed a different theme.
        let original = crate::feat::theme::default_theme();
        let mut other = original.clone();
        other.focus_accent = ratatui::style::Color::Red;
        let mut state = state_with_themes(&[("other", other.clone())]);
        open(&mut state);
        state.frontend.theme = other;
        let registry = crate::feat::picker::registry::build_picker_registry();
        let picker_id = PickerId::new(crate::feat::picker::registry::THEME_ID);

        // When closing via the spec's close hook.
        let outcome = {
            let mut host = crate::feat::picker::host_impl::AppStatePickerHost::new(&mut state);
            let mut ctx = ActionCtx::new(picker_id, &mut host);
            let spec = registry
                .get(crate::feat::picker::registry::THEME_ID)
                .expect("theme spec registered");
            spec.run_close(&mut ctx)
        };

        // Then the pre-open theme is restored.
        assert_eq!(state.frontend.theme.focus_accent, original.focus_accent);
        // And the snapshot is consumed.
        assert!(state.frontend.theme_preview_original().is_none());
        // And the picker closes.
        assert!(outcome.close);
    }

    #[rstest::rstest]
    #[test]
    fn status_renders_the_persisted_theme_name() {
        // Given a state whose persisted theme is "gruvbox-dark".
        let state = AppState::default();
        let registry = crate::feat::picker::registry::build_picker_registry();
        let spec = registry
            .get(crate::feat::picker::registry::THEME_ID)
            .expect("theme spec registered");

        // When rendering the status line (with no persisted theme, the
        // built-in default is in force).
        let rendered = {
            let host = crate::feat::picker::host_impl::AppStateRenderHost::new(&state);
            let ctx = jinn_picker::StatusCtx::new(
                PickerId::new(crate::feat::picker::registry::THEME_ID),
                &host,
            );
            spec.status_line(&ctx)
        };

        // Then the status names the default theme.
        let line = rendered.expect("status declared");
        let text: String = line.spans.iter().map(|s| s.content.to_string()).collect();
        assert_eq!(text, "Current: default");
    }
}
