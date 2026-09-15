//! Picker overlay rendering - dispatches to domain-specific picker renderers.

use jinn_domain::PickerKind;
use jinn_domain::RenderCtx;
use ratatui::Frame;
use ratatui::layout::Rect;

/// Renders the active picker overlay, dispatching on [`PickerKind`].
pub(super) fn render_picker(frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx) {
    // Spec-driven kinds render through the registry first; the legacy arms
    // below stay authoritative for kinds without a spec.
    if let Some(kind) = ctx.state.frontend.scope_stack.picker_kind().copied()
        && let Some(id) = jinn_domain::feat::picker::registry::spec_id_for_kind(&kind)
        && let Some(spec) = ctx.pickers.get(id)
    {
        let host = jinn_domain::feat::picker::host_impl::AppStateRenderHost::new(ctx.state);
        if spec.render(frame, area, &host) {
            return;
        }
        // The spec rendered nothing (storage not wrapped yet) — fall
        // through to the legacy renderer below.
    }
    match ctx.state.frontend.scope_stack.picker_kind().copied() {
        Some(PickerKind::Provider) => render_provider_picker(frame, area, ctx),
        Some(PickerKind::Session) => render_session_picker(frame, area, ctx),
        Some(PickerKind::Endpoint) => {
            jinn_domain::feat::endpoint::picker_render::render_endpoint_picker(frame, area, ctx);
        }
        // Persona, Skill, Theme, Tool, McpServer, SessionLifecycle, and
        // ReasoningEffort render entirely through their specs above; with an
        // empty registry (test seams) there is nothing to draw. `None` (no
        // picker scope) is also a no-op here.
        Some(
            PickerKind::Persona
            | PickerKind::Skill
            | PickerKind::Theme
            | PickerKind::Tool
            | PickerKind::McpServer
            | PickerKind::SessionLifecycle
            | PickerKind::ReasoningEffort,
        )
        | None => {}
        Some(PickerKind::TaskList) => {
            jinn_domain::feat::picker::render::render_task_list_picker(frame, area, ctx);
        }
        Some(PickerKind::Project) => {
            jinn_domain::feat::picker::render::render_project_picker(frame, area, ctx);
        }
        Some(PickerKind::Plugin) => {
            jinn_domain::feat::picker::render::render_plugin_picker(frame, area, ctx);
        }
    }
}

/// Renders the provider picker overlay (delegates to slice).
fn render_provider_picker(frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx) {
    jinn_domain::feat::provider::render::render_provider_picker(frame, area, ctx);
}

/// Renders the session picker overlay (delegates to slice).
fn render_session_picker(frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx) {
    jinn_domain::feat::session::render::render_session_picker(frame, area, ctx);
}

/// Renders the session lifecycle picker overlay (delegates to domain render).

/// Renders the arg input popup (delegates to domain render).
pub(super) fn render_arg_input(frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx) {
    jinn_domain::feat::session_lifecycle::render::render_arg_input(frame, area, ctx);
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::indexing_slicing,
        reason = "test code, panics are acceptable"
    )]
    use jinn_domain::AppState;
    use jinn_domain::FocusScope;
    use jinn_domain::PickerKind;
    use jinn_domain::feat::ui::picker_states::PickerExt as _;
    use jinn_selection_widget::compute_popup_rect;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;

    #[rstest::rstest]
    fn larger_terminal_gets_taller_popup() {
        // Given two terminal sizes.
        let small_area = Rect::new(0, 0, 80, 24);
        let large_area = Rect::new(0, 0, 80, 42);

        // When computing popup rects.
        let small_popup = compute_popup_rect(small_area);
        let large_popup = compute_popup_rect(large_area);

        // Then the larger terminal gets a taller popup.
        assert!(large_popup.height > small_popup.height);
    }

    #[rstest::rstest]
    fn small_terminal_uses_75_percent_height() {
        // Given two terminal sizes.
        let small_area = Rect::new(0, 0, 80, 24);
        let large_area = Rect::new(0, 0, 80, 42);

        // When computing popup rects.
        let small_popup = compute_popup_rect(small_area);
        let _large_popup = compute_popup_rect(large_area);

        // Then the small terminal popup uses 75% of height + 4 rows of chrome.
        // floor(24 * 0.75) = 18, min(18 + 4, 24) = 22.
        assert_eq!(small_popup.height, 22);
    }

    /// Each picker kind must draw exactly the number of footer rows it
    /// advertises via [`PickerKind::footer_rows`]. This is the drift-prevention
    /// backstop for the picker viewport measurement: if a render site ever
    /// adds or drops a footer without updating `footer_rows()`, the geometry
    /// helper would reserve the wrong number of rows and the cursor could drift
    /// off-screen. With an empty item list, the results area is blank, so the
    /// consecutive non-blank rows at the bottom of the popup's inner area equal
    /// the footer count actually drawn.
    #[rstest::rstest]
    #[case::provider(PickerKind::Provider)]
    #[case::session(PickerKind::Session)]
    #[case::persona(PickerKind::Persona)]
    #[case::theme(PickerKind::Theme)]
    #[case::session_lifecycle(PickerKind::SessionLifecycle)]
    #[case::reasoning_effort(PickerKind::ReasoningEffort)]
    #[case::endpoint(PickerKind::Endpoint)]
    #[case::tool(PickerKind::Tool)]
    #[case::skill(PickerKind::Skill)]
    #[case::task_list(PickerKind::TaskList)]
    #[case::project(PickerKind::Project)]
    #[case::mcp_server(PickerKind::McpServer)]
    #[case::plugin(PickerKind::Plugin)]
    fn picker_draws_footer_rows_matching_kind_declaration(#[case] kind: PickerKind) {
        // Given a picker scope of this kind with the default (empty) state,
        // and the domain's picker registry.
        let mut state = AppState::default();
        state.frontend.scope_stack.push(FocusScope::Picker { kind });
        let pickers = jinn_domain::feat::picker::registry::build_picker_registry();

        // When rendering the picker overlay.
        let area = Rect::new(0, 0, 100, 30);
        let mut terminal =
            Terminal::new(TestBackend::new(area.width, area.height)).expect("terminal");
        terminal
            .draw(|frame| {
                let slices = jinn_slices::Slices::new();
                let views = jinn_domain::common::overlay_views::OverlayViews::new();
                let ctx =
                    jinn_domain::RenderCtx::new(&state, &slices, &views).with_pickers(&pickers);
                super::render_picker(frame, area, &ctx);
            })
            .expect("draw");

        // Then the number of footer rows actually drawn equals the kind's declaration.
        let popup = compute_popup_rect(area);
        // Inner popup area excludes the border.
        let inner_top = popup.y + 1;
        let inner_bottom = popup.y + popup.height.saturating_sub(2);
        let inner_x_start = popup.x + 1;
        let inner_x_end = popup.x + popup.width.saturating_sub(1);

        let buffer = terminal.backend().buffer();
        let row_is_blank = |y: u16| -> bool {
            (inner_x_start..inner_x_end).all(|x| buffer[(x, y)].symbol().trim().is_empty())
        };

        // Count consecutive non-blank rows climbing up from the bottom of the
        // popup. With an empty results list this is exactly the footer block.
        let mut drawn_footer_rows = 0u16;
        for y in (inner_top..=inner_bottom).rev() {
            if row_is_blank(y) {
                break;
            }
            drawn_footer_rows += 1;
        }

        // The declared footer count comes from the spec when the kind has
        // one (bottom rows are spec-owned), else from the legacy kind.
        let declared = jinn_domain::feat::picker::registry::spec_id_for_kind(&kind)
            .and_then(|id| pickers.get(id))
            .map_or_else(|| kind.footer_rows(), |spec| spec.bottom_rows());

        assert_eq!(
            drawn_footer_rows, declared,
            "picker {kind} draws {drawn_footer_rows} footer rows but declares {declared}",
        );
    }

    #[rstest::rstest]
    #[test]
    fn persona_picker_draws_status_and_keybind_rows_via_spec() {
        // Given a persona picker open, rendered through its spec.
        let mut state = AppState::default();
        state.frontend.scope_stack.push(FocusScope::Picker {
            kind: PickerKind::Persona,
        });
        let pickers = jinn_domain::feat::picker::registry::build_picker_registry();

        // When rendering.
        let area = Rect::new(0, 0, 100, 30);
        let mut terminal =
            Terminal::new(TestBackend::new(area.width, area.height)).expect("terminal");
        terminal
            .draw(|frame| {
                let slices = jinn_slices::Slices::new();
                let views = jinn_domain::common::overlay_views::OverlayViews::new();
                let ctx =
                    jinn_domain::RenderCtx::new(&state, &slices, &views).with_pickers(&pickers);
                super::render_picker(frame, area, &ctx);
            })
            .expect("draw");

        // Then the popup draws the spec's two bottom rows: the "Active:"
        // status line above the standard keybind line.
        let popup = compute_popup_rect(area);
        let inner_bottom = popup.y + popup.height.saturating_sub(2);
        let buffer = terminal.backend().buffer();
        let keybind_row: String = ((popup.x + 1)..(popup.x + popup.width - 1))
            .map(|x| buffer[(x, inner_bottom)].symbol())
            .collect();
        let status_row: String = ((popup.x + 1)..(popup.x + popup.width - 1))
            .map(|x| buffer[(x, inner_bottom - 1)].symbol())
            .collect();
        assert!(
            keybind_row.contains("Enter confirm"),
            "bottom row must be the keybind line; got {keybind_row:?}"
        );
        assert!(
            status_row.contains("Active:"),
            "row above must be the status line; got {status_row:?}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn theme_picker_draws_status_and_keybind_rows_via_spec() {
        // Given a theme picker open, rendered through its spec.
        let mut state = AppState::default();
        state.frontend.scope_stack.push(FocusScope::Picker {
            kind: PickerKind::Theme,
        });
        let pickers = jinn_domain::feat::picker::registry::build_picker_registry();

        // When rendering.
        let area = Rect::new(0, 0, 100, 30);
        let mut terminal =
            Terminal::new(TestBackend::new(area.width, area.height)).expect("terminal");
        terminal
            .draw(|frame| {
                let slices = jinn_slices::Slices::new();
                let views = jinn_domain::common::overlay_views::OverlayViews::new();
                let ctx =
                    jinn_domain::RenderCtx::new(&state, &slices, &views).with_pickers(&pickers);
                super::render_picker(frame, area, &ctx);
            })
            .expect("draw");

        // Then the popup draws the spec's two bottom rows: the "Current:"
        // status line above the standard keybind line.
        let popup = compute_popup_rect(area);
        let inner_bottom = popup.y + popup.height.saturating_sub(2);
        let buffer = terminal.backend().buffer();
        let keybind_row: String = ((popup.x + 1)..(popup.x + popup.width - 1))
            .map(|x| buffer[(x, inner_bottom)].symbol())
            .collect();
        let status_row: String = ((popup.x + 1)..(popup.x + popup.width - 1))
            .map(|x| buffer[(x, inner_bottom - 1)].symbol())
            .collect();
        assert!(
            keybind_row.contains("Enter confirm"),
            "bottom row must be the keybind line; got {keybind_row:?}"
        );
        assert!(
            status_row.contains("Current: default"),
            "row above must be the status line; got {status_row:?}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn mcp_picker_draws_status_and_keybind_rows_via_spec() {
        // Given an MCP server picker open, rendered through its spec.
        let mut state = AppState::default();
        state.frontend.scope_stack.push(FocusScope::Picker {
            kind: PickerKind::McpServer,
        });
        let pickers = jinn_domain::feat::picker::registry::build_picker_registry();

        // When rendering.
        let area = Rect::new(0, 0, 100, 30);
        let mut terminal =
            Terminal::new(TestBackend::new(area.width, area.height)).expect("terminal");
        terminal
            .draw(|frame| {
                let slices = jinn_slices::Slices::new();
                let views = jinn_domain::common::overlay_views::OverlayViews::new();
                let ctx =
                    jinn_domain::RenderCtx::new(&state, &slices, &views).with_pickers(&pickers);
                super::render_picker(frame, area, &ctx);
            })
            .expect("draw");

        // Then the popup draws the spec's two bottom rows: the "0/0 enabled"
        // status line above the standard keybind line.
        let popup = compute_popup_rect(area);
        let inner_bottom = popup.y + popup.height.saturating_sub(2);
        let buffer = terminal.backend().buffer();
        let keybind_row: String = ((popup.x + 1)..(popup.x + popup.width - 1))
            .map(|x| buffer[(x, inner_bottom)].symbol())
            .collect();
        let status_row: String = ((popup.x + 1)..(popup.x + popup.width - 1))
            .map(|x| buffer[(x, inner_bottom - 1)].symbol())
            .collect();
        assert!(
            keybind_row.contains("Enter confirm"),
            "bottom row must be the keybind line; got {keybind_row:?}"
        );
        assert!(
            status_row.contains("0/0 enabled"),
            "row above must be the status line; got {status_row:?}"
        );
        // And the keybind line advertises the spec's custom binds (the
        // generator echoes each row's raw notation + label).
        assert!(
            keybind_row.contains("<tab> toggle")
                && keybind_row.contains("<c-r> restart")
                && keybind_row.contains("<c-t> logs/tools"),
            "keybind line must list the spec's binds; got {keybind_row:?}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn theme_picker_draws_swatch_rows_via_the_spec_row_hook() {
        // Given an open theme picker whose storage holds wrapped entries
        // (the same shape the spec's open hook produces).
        let mut state = AppState::default();
        state.frontend.scope_stack.push(FocusScope::Picker {
            kind: PickerKind::Theme,
        });
        let pickers = jinn_domain::feat::picker::registry::build_picker_registry();
        let wrapped = pickers
            .make_items(
                jinn_domain::feat::picker::registry::THEME_ID,
                vec![jinn_domain::feat::theme::ThemeEntry {
                    name: "gruvbox".to_owned(),
                    theme: state.frontend.theme.clone(),
                }],
            )
            .expect("theme spec registered");
        state.frontend.theme_picker_mut().set_items(wrapped);

        // When rendering the picker.
        let area = Rect::new(0, 0, 100, 30);
        let mut terminal =
            Terminal::new(TestBackend::new(area.width, area.height)).expect("terminal");
        terminal
            .draw(|frame| {
                let slices = jinn_slices::Slices::new();
                let views = jinn_domain::common::overlay_views::OverlayViews::new();
                let ctx =
                    jinn_domain::RenderCtx::new(&state, &slices, &views).with_pickers(&pickers);
                super::render_picker(frame, area, &ctx);
            })
            .expect("draw");

        // Then the entry's swatch and name appear — rows are not blank and
        // the name is drawn.
        let rendered: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(
            rendered.contains('\u{2588}') && rendered.contains("gruvbox"),
            "theme picker must draw its swatch + name rows; got {rendered:?}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn persona_picker_draws_entry_rows_via_the_spec_row_hook() {
        // Given an open persona picker whose storage holds wrapped entries
        // (the same shape the session actor's loader produces).
        let mut state = AppState::default();
        state.frontend.scope_stack.push(FocusScope::Picker {
            kind: PickerKind::Persona,
        });
        let pickers = jinn_domain::feat::picker::registry::build_picker_registry();
        let wrapped = pickers
            .make_items(
                jinn_domain::feat::picker::registry::PERSONA_ID,
                vec![jinn_domain::feat::persona::PersonaEntry {
                    name: "coder".to_owned(),
                    description: "code helper".to_owned(),
                    is_active: false,
                    theme: state.frontend.theme.clone(),
                }],
            )
            .expect("persona spec registered");
        state.frontend.persona_picker_mut().set_items(wrapped);

        // When rendering the picker.
        let area = Rect::new(0, 0, 100, 30);
        let mut terminal =
            Terminal::new(TestBackend::new(area.width, area.height)).expect("terminal");
        terminal
            .draw(|frame| {
                let slices = jinn_slices::Slices::new();
                let views = jinn_domain::common::overlay_views::OverlayViews::new();
                let ctx =
                    jinn_domain::RenderCtx::new(&state, &slices, &views).with_pickers(&pickers);
                super::render_picker(frame, area, &ctx);
            })
            .expect("draw");

        // Then the entry's name appears in the popup — rows are not blank.
        let rendered: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(
            rendered.contains("coder"),
            "persona picker must draw its entry rows; got {rendered:?}"
        );
    }
}
