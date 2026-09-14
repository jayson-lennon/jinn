//! Picker overlay rendering - dispatches to domain-specific picker renderers.

use jinn_domain::PickerKind;
use jinn_domain::RenderCtx;
use ratatui::Frame;
use ratatui::layout::Rect;

/// Renders the active picker overlay, dispatching on [`PickerKind`].
pub(super) fn render_picker(frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx) {
    match ctx.state.frontend.scope_stack.picker_kind().copied() {
        Some(PickerKind::Provider) => render_provider_picker(frame, area, ctx),
        Some(PickerKind::Session) => render_session_picker(frame, area, ctx),
        Some(PickerKind::Persona) => render_persona_picker(frame, area, ctx),
        Some(PickerKind::Theme) => render_theme_picker(frame, area, ctx),
        Some(PickerKind::SessionLifecycle) => {
            render_session_lifecycle_picker(frame, area, ctx);
        }
        Some(PickerKind::CompactionModel) => {
            jinn_domain::feat::provider::render::render_compaction_model_picker(frame, area, ctx);
        }
        Some(PickerKind::ReasoningEffort) => {
            jinn_domain::feat::reasoning::picker_render::render_reasoning_effort_picker(
                frame, area, ctx,
            );
        }
        Some(PickerKind::Endpoint) => {
            jinn_domain::feat::endpoint::picker_render::render_endpoint_picker(frame, area, ctx);
        }
        Some(PickerKind::Tool) => {
            jinn_domain::feat::picker::render::render_tool_picker(frame, area, ctx);
        }
        Some(PickerKind::Skill) => {
            jinn_domain::feat::picker::render::render_skill_picker(frame, area, ctx);
        }
        Some(PickerKind::TaskList) => {
            jinn_domain::feat::picker::render::render_task_list_picker(frame, area, ctx);
        }
        Some(PickerKind::Project) => {
            jinn_domain::feat::picker::render::render_project_picker(frame, area, ctx);
        }
        Some(PickerKind::McpServer) => {
            jinn_domain::feat::mcp::render::render_mcp_server_picker(frame, area, ctx);
        }
        Some(PickerKind::Plugin) => {
            jinn_domain::feat::picker::render::render_plugin_picker(frame, area, ctx);
        }
        None => {}
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

/// Renders the persona picker overlay (delegates to domain render).
fn render_persona_picker(frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx) {
    jinn_domain::feat::picker::render::render_persona_picker(frame, area, ctx);
}

/// Renders the theme picker overlay (delegates to domain render).
fn render_theme_picker(frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx) {
    jinn_domain::feat::picker::render::render_theme_picker(frame, area, ctx);
}

/// Renders the session lifecycle picker overlay (delegates to domain render).
fn render_session_lifecycle_picker(frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx) {
    jinn_domain::feat::session_lifecycle::render::render_session_lifecycle_picker(frame, area, ctx);
}

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
    #[case::compaction_model(PickerKind::CompactionModel)]
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
                let ctx = jinn_domain::RenderCtx::new(&state, &slices, &views)
                    .with_pickers(&pickers);
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
            .map(|spec| spec.bottom_rows())
            .unwrap_or_else(|| kind.footer_rows());

        assert_eq!(
            drawn_footer_rows,
            declared,
            "picker {kind} draws {drawn_footer_rows} footer rows but declares {declared}",
        );
    }
}
