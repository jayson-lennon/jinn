//! The skill picker's overlay rendering.
//!
//! A slice-owned picker renders from the slice's own cell. The render
//! context deliberately never sees `AppState` (see `RenderFacts`), so this
//! reads `SkillPickerState` straight off the registered slot and drives the
//! preview selection widget itself — no `PickerHost`, no kernel borrow, and
//! no picker-kind lookup in the kernel.

use jinn_selection_widget::PreviewSelectionWidget;
use jinn_skills_msg::SkillPickerState;
use jinn_slices::RenderFacts;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

/// The popup rect for the skill picker: the shared selection-widget popup.
#[must_use]
pub fn skill_picker_overlay_rect(area: &Rect) -> Option<Rect> {
    Some(jinn_selection_widget::compute_popup_rect(*area))
}

/// The colors the skill picker draws with.
///
/// Mirrors the widget's defaults, matching what the kernel-side spec render
/// produced: the chrome fields are unthemed, the accent fields come from the
/// active theme.
#[must_use]
pub fn skill_picker_palette(theme: &jinn_theme::Theme) -> jinn_picker::Palette {
    jinn_picker::Palette {
        border: ratatui::style::Color::DarkGray,
        filter_text: ratatui::style::Color::White,
        separator: ratatui::style::Color::DarkGray,
        footer: ratatui::style::Color::DarkGray,
        highlight_bg: ratatui::style::Color::DarkGray,
        muted_text: theme.muted_text,
        accent_action: theme.accent_action,
        popup_title: theme.popup_title,
        primary_text: theme.primary_text,
    }
}

/// The skill picker's declared binds, in footer order.
///
/// Sourced from the route rows the slice actually attaches, so the footer can
/// never advertise a key the picker does not bind.
#[must_use]
pub fn skill_picker_binds() -> Vec<jinn_picker::BindRow> {
    crate::skill_picker_routes::SKILL_PICKER_BINDINGS
        .iter()
        .map(|(notation, label)| jinn_picker::BindRow {
            notation,
            label,
            category_hint: "input",
        })
        .collect()
}

/// The cost status line: the live enabled count and the two-term context
/// figure, e.g. `3/7 enabled · ctx: 1.2k list + 8.4k loaded`.
///
/// The **list** term is the pending `<available_skills>` block the enabled
/// rows would ship; the **loaded** term is the pinned skill bodies already in
/// context. Both are always shown — a loaded skill disabled in the picker
/// still costs its body, and the breakdown is what makes that visible.
#[must_use]
pub fn cost_status(state: &SkillPickerState, theme: &jinn_theme::Theme) -> Line<'static> {
    let (enabled, total) = crate::skill_picker_actions::enabled_count(state);
    let list = jinn_picker::format_compact(state.list_tokens);
    let loaded = jinn_picker::format_compact(state.loaded_tokens);
    Line::from(Span::styled(
        format!("{enabled}/{total} enabled · ctx: {list} list + {loaded} loaded"),
        Style::default().fg(theme.muted_text),
    ))
}

/// Draws the skill picker popup for one frame.
pub fn render_skill_picker(frame: &mut Frame<'_>, area: Rect, facts: &RenderFacts) {
    let Some(cell) = facts.slices.reader(&jinn_skills_msg::skill_picker_slot()) else {
        return;
    };

    let theme = &facts.theme;
    let palette = skill_picker_palette(theme);
    let binds = skill_picker_binds();
    let keybind = jinn_picker::keybind_line(&binds, jinn_picker::Tail::Standard, &palette);

    // Render under the read guard, then publish the layout after the block
    // ends: `TypedCell`'s `parking_lot::RwLock` is not reentrant, and a
    // same-thread write while the read guard is alive deadlocks.
    let (layout, preview_report) = {
        let guard = cell.read();
        let state: &SkillPickerState = &guard;
        let footers = vec![cost_status(state, theme), Line::from(keybind.0)];

        let mut report = None;
        let widget = PreviewSelectionWidget::new(&state.selection)
            .title(Line::from(" Skills "))
            .title_style(Style::default().fg(theme.popup_title))
            .footers(footers)
            .colors(palette.selection_colors())
            .preview_scroll(state.preview_scroll)
            .preview_report(&mut report);

        let cache = state.preview_cache.clone();
        let layout = widget.preview_cache(cache.as_ref()).render(frame, area);
        (layout, report)
    };

    // The widget reports the rects it actually solved, in whichever split
    // mode it chose for this popup; the navigation keys page against that row
    // count. This is the slice-owned equivalent of the kernel's per-frame
    // viewport write — measured by the widget itself rather than mirrored
    // arithmetic. The preview report rides the same update: its `max_scroll`
    // is the clamp bound for the stored preview offset, and clamping here
    // kills a stale oversized value (persisted session, filter narrowing) on
    // the first frame after it appears.
    cell.update(|state: &mut SkillPickerState| {
        state.results_viewport = layout.inner.height as usize;
        if let Some(report) = preview_report {
            state.preview_max_scroll = report.max_scroll();
            state.preview_scroll = state.preview_scroll.min(state.preview_max_scroll);
        }
    });
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        reason = "test module, panics are acceptable"
    )]
    use super::*;
    use jinn_slices::OverlayViews;
    use jinn_slices::SliceHost;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    /// An activated skills slice whose picker cell holds one `web-coder` item.
    async fn slices_with_one_skill() -> jinn_slices::Slices {
        let slices = jinn_slices::Slices::new();
        // Production boot seeds every slice cell before any slice activates;
        // `activate` resolves the picker's cell by slot key.
        jinn_cell_catalog::register_all_cells(&slices);
        let mut viewport = jinn_slices::view::Viewport::new();
        let overlay_views = OverlayViews::new();
        let key_routes = jinn_slices::KeyRoutes::new();
        let services = jinn_kernel::Services::new_fake().await;
        let mut host = SliceHost::new(
            &slices,
            &mut viewport,
            &overlay_views,
            &key_routes,
            &services.trouper_system,
        );
        crate::activate(&mut host);
        slices
            .reader::<jinn_skills_msg::SkillPickerState>(&jinn_skills_msg::skill_picker_slot())
            .expect("cell registered")
            .update(|state: &mut jinn_skills_msg::SkillPickerState| {
                state
                    .selection
                    .set_items(jinn_picker::make_items_with_hooks(
                        vec![jinn_skills_msg::SkillEntry {
                            name: "web-coder".to_owned(),
                            description: "writes web code".to_owned(),
                            body: "# Body".to_owned(),
                            enabled: true,
                            source: jinn_skills_msg::SkillSource::Global,
                            theme: jinn_theme::default_theme(),
                        }],
                        jinn_picker::PickerItemHooks::new()
                            .row(jinn_skills_msg::skill_row)
                            .search(|e: &jinn_skills_msg::SkillEntry| e.name.clone()),
                    ));
            });
        slices
    }

    /// The load-bearing test for a slice-owned picker: the render pass must
    /// draw it. The kernel dispatches a `FocusScope::Dynamic` picker through
    /// the generic overlay path, so this exercises geometry + view + cell
    /// with no kernel borrow at all.
    #[rstest::rstest]
    #[tokio::test]
    async fn render_draws_the_skill_picker_from_its_own_cell() {
        // Given an activated skills slice holding one skill.
        let slices = slices_with_one_skill().await;
        let facts = jinn_slices::RenderFacts::new(jinn_theme::default_theme(), &slices);

        // When the overlay view renders the picker.
        let area = Rect::new(0, 0, 100, 30);
        let mut terminal =
            Terminal::new(TestBackend::new(area.width, area.height)).expect("terminal");
        terminal
            .draw(|frame| {
                let popup = skill_picker_overlay_rect(&area).expect("geometry fn yields a rect");
                render_skill_picker(frame, popup, &facts);
            })
            .expect("draw");

        // Then the skill's name is on screen — the picker draws itself.
        let rendered: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(
            rendered.contains("web-coder"),
            "slice-owned picker must draw its rows; got {rendered:?}"
        );
    }

    /// The overlay must be discoverable by identity alone: the render pass
    /// looks it up through the registry with no picker-specific branch.
    #[rstest::rstest]
    #[tokio::test]
    async fn registered_overlay_resolves_by_scope_identity() {
        // Given an activated skills slice.
        let slices = jinn_slices::Slices::new();
        jinn_cell_catalog::register_all_cells(&slices);
        let mut viewport = jinn_slices::view::Viewport::new();
        let overlay_views = OverlayViews::new();
        let key_routes = jinn_slices::KeyRoutes::new();
        let services = jinn_kernel::Services::new_fake().await;
        let mut host = SliceHost::new(
            &slices,
            &mut viewport,
            &overlay_views,
            &key_routes,
            &services.trouper_system,
        );
        crate::activate(&mut host);
        let scope = crate::skill_picker_scope();

        // When the render pass resolves the scope's overlay and view.
        let overlay = slices.overlay(&scope);
        let view = overlay_views.view(&scope);
        let area = Rect::new(0, 0, 100, 30);

        // Then both are present and the geometry yields a popup rect.
        assert!(overlay.is_some(), "overlay geometry must be registered");
        assert!(view.is_some(), "overlay view must be registered");
        let rect = overlay.expect("overlay")(&area).expect("rect");
        assert!(rect.width > 0 && rect.height > 0);
    }
}
