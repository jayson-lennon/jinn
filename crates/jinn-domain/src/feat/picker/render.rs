//! Picker rendering for persona and theme pickers.

use crate::common::render_ctx::RenderCtx;
use crate::feat::ui::picker_states::PickerExt;
use jinn_selection_widget::SelectionWidget;
use jinn_selection_widget::TreePickerWidget;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;

/// Renders the persona picker overlay using [`SelectionWidget`].
///
/// Telescope-style layout: bordered popup with filter input at top,
/// horizontal separator, scrollable persona entries.
pub fn render_persona_picker(frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx) {
    let state = ctx.state;
    let footer = {
        use ratatui::style::Style;
        use ratatui::text::{Line, Span};
        let gray = Style::default().fg(state.frontend.theme.muted_text);
        let active_name = state
            .context
            .active_persona()
            .map_or("none", |p| p.name.as_str());
        Line::from(vec![
            Span::styled("Active: ".to_owned(), gray),
            Span::styled(
                active_name.to_owned(),
                Style::default().fg(state.frontend.theme.primary_text),
            ),
        ])
    };
    let widget = SelectionWidget::new(state.frontend.persona_picker())
        .title(Line::from(" Personas "))
        .title_style(Style::default().fg(state.frontend.theme.popup_title))
        .footer(footer);
    widget.render(frame, area);
}

/// Renders the theme picker overlay using [`SelectionWidget`].
///
/// Telescope-style layout: bordered popup with filter input at top,
/// horizontal separator, scrollable theme entries.
pub fn render_theme_picker(frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx) {
    let state = ctx.state;
    let widget = SelectionWidget::new(state.frontend.theme_picker())
        .title(Line::from(" Themes "))
        .title_style(Style::default().fg(state.frontend.theme.popup_title))
        .footer(Line::from(" ESC to cancel, Enter to apply "));
    widget.render(frame, area);
}

/// Renders the tool picker overlay.
pub fn render_tool_picker(frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx) {
    let state = ctx.state;
    let enabled_count = state
        .frontend
        .tool_picker()
        .items()
        .iter()
        .filter(|t| t.enabled)
        .count();
    let total = state.frontend.tool_picker().items().len();
    let footer = Line::from(format!(
        " TAB toggle \u{00b7} {enabled_count}/{total} enabled \u{00b7} Enter confirm \u{00b7} ESC cancel "
    ));
    let widget = SelectionWidget::new(state.frontend.tool_picker())
        .title(Line::from(" Tools "))
        .title_style(Style::default().fg(state.frontend.theme.popup_title))
        .footer(footer);
    widget.render(frame, area);
}

/// Renders the plugin picker overlay — a read-only list of loaded plugins.
///
/// One row per plugin (name + lifecycle phase). Plugins are managed outside
/// jinn, so the footer only hints navigation: no toggle, no confirm action.
pub fn render_plugin_picker(frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx) {
    let state = ctx.state;
    let footer = Line::from(" Read-only \u{00b7} filter to narrow \u{00b7} ESC close ");
    let widget = SelectionWidget::new(state.frontend.plugin_picker())
        .title(Line::from(" Plugins "))
        .title_style(Style::default().fg(state.frontend.theme.popup_title))
        .footer(footer);
    widget.render(frame, area);
}

/// Renders the read-only task list browser overlay.
///
/// Uses [`TreePickerWidget`] to show phases as roots and tasks as their children,
/// fully expanded. Enter is a no-op; ESC/Q close the picker.
pub fn render_task_list_picker(frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx) {
    let state = ctx.state;
    let gray = Style::default().fg(state.frontend.theme.muted_text);
    let footer = Line::from(vec![ratatui::text::Span::styled(
        "ESC to close \u{00b7} Enter no-op \u{00b7} type to filter".to_owned(),
        gray,
    )]);
    let widget = TreePickerWidget::new(state.frontend.task_list_picker())
        .title(Line::from(" Task List "))
        .title_style(Style::default().fg(state.frontend.theme.popup_title))
        .footer(footer);
    widget.render(frame, area);
}

/// Renders the project picker overlay using [`SelectionWidget`].
///
/// Shows curated project directories. The footer advertises the three
/// project-picker actions: `<enter>` (new session at dir), `<c-enter>`
/// (new session at dir + lifecycle picker), and `<c-n>` (add a new project dir).
pub fn render_project_picker(frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx) {
    let state = ctx.state;
    let gray = Style::default().fg(state.frontend.theme.muted_text);
    let orange = Style::default().fg(state.frontend.theme.accent_action);
    let footer = Line::from(vec![
        ratatui::text::Span::styled("Enter ", orange),
        ratatui::text::Span::styled("new session \u{00b7} ", gray),
        ratatui::text::Span::styled("<c-enter> ", orange),
        ratatui::text::Span::styled("new + lifecycle \u{00b7} ", gray),
        ratatui::text::Span::styled("<c-n> ", orange),
        ratatui::text::Span::styled("add dir \u{00b7} ", gray),
        ratatui::text::Span::styled("<c-d> ", orange),
        ratatui::text::Span::styled("remove \u{00b7} ", gray),
        ratatui::text::Span::styled("ESC ", orange),
        ratatui::text::Span::styled("to cancel", gray),
    ]);
    let widget = SelectionWidget::new(state.frontend.project_picker())
        .title(Line::from(" Projects "))
        .title_style(Style::default().fg(state.frontend.theme.popup_title))
        .footer(footer);
    widget.render(frame, area);
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic,
        clippy::unreachable,
        clippy::string_slice,
        clippy::uninlined_format_args,
        reason = "test code"
    )]
    use super::*;
    use crate::common::app_state::{AppState, FocusScope};
    use crate::common::render_ctx::RenderCtx;
    use crate::feat::skills::reload::reload_skill_picker_entries;
    use crate::feat::ui::picker_states::PickerExt;
    use crate::protocol::PickerKind;

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    /// Renders the skill picker through its registered spec (the same path
    /// the tui render pass takes for migrated kinds).
    fn render_skill_picker(frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx) {
        let host = crate::feat::picker::host_impl::AppStateRenderHost::new(ctx.state);
        let id = crate::feat::picker::registry::spec_id_for_kind(&PickerKind::Skill)
            .expect("skill is spec-mapped");
        let spec = ctx
            .pickers
            .get(id)
            .expect("skill spec registered in the domain registry");
        assert!(spec.render(frame, area, &host), "spec render must drive the preview widget");
    }

    /// Two skills with the same name but different bodies (the cross-session
    /// project/global shadowing shape) must occupy distinct cache entries — the
    /// body-hash key means neither can ever be served the other's markdown.
    #[rstest::rstest]
    #[test]
    fn render_skill_picker_same_name_different_bodies_cache_independently() {
        // Given a picker holding two same-named skills with different bodies
        // (as two sessions' shadowing would produce).
        let mut state = AppState::default();
        let skill = |body: &str| crate::feat::skills::Skill {
            name: "shared".to_owned(),
            description: "shadowed".to_owned(),
            body: body.to_owned(),
            file_path: std::path::PathBuf::from("/tmp/shared/SKILL.md"),
            base_dir: std::path::PathBuf::from("/tmp/shared"),
            source: crate::feat::skills::SkillSource::Global,
        };
        state
            .active_session_mut()
            .set_discovered_skills(vec![skill("# GLOBAL body"), skill("# PROJECT body")]);
        state.frontend.scope_stack.push(FocusScope::Picker {
            kind: PickerKind::Skill,
        });
        {
            let disabled = state.active_session().disabled_skills().clone();
            let theme = state.frontend.theme.clone();
            let discovered = state.active_session().discovered_skills().to_vec();
            reload_skill_picker_entries(&mut state.frontend, &discovered, &disabled, &theme);
        }
        state.frontend.skill_picker_mut().set_selection(0);

        let draw = |state: &AppState, area: Rect| {
            let backend = TestBackend::new(area.width, area.height);
            let mut terminal = Terminal::new(backend).expect("terminal");
            terminal
                .draw(|frame| {
                    let slices = jinn_slices::Slices::new();
                    let overlay_views = crate::common::overlay_views::OverlayViews::new();
                    let ctx = RenderCtx::new(state, &slices, &overlay_views)
                        .with_pickers(&crate::feat::picker::registry::build_picker_registry());
                    render_skill_picker(frame, area, &ctx);
                })
                .expect("draw");
        };

        // When rendering each same-named skill at the same terminal size.
        let area = Rect::new(0, 0, 100, 30);
        draw(&state, area);
        let after_first = state.frontend.caches.skill_preview_cache.len();
        state.frontend.skill_picker_mut().set_selection(1);
        draw(&state, area);
        let after_second = state.frontend.caches.skill_preview_cache.len();

        // Then the second body adds a second entry — no (name, width) collision.
        assert_eq!(after_first, 1, "first body populates one entry");
        assert_eq!(
            after_second, 2,
            "different body under the same name must be a cache miss, not a collision"
        );
    }

    /// Rendering the skill picker with the same selection and width populates the
    /// preview cache exactly once; the second render is a cache hit (no re-render).
    #[rstest::rstest]
    #[test]
    fn render_skill_picker_caches_preview_per_skill_and_width() {
        // Given a picker populated with two skills and a selection on the first.
        let mut state = AppState::default();
        state
            .active_session_mut()
            .set_discovered_skills(vec![crate::feat::skills::Skill {
                name: "web-coder".to_owned(),
                description: "Web coder".to_owned(),
                body: "## Body text that renders".to_owned(),
                file_path: std::path::PathBuf::from("/tmp/web-coder/SKILL.md"),
                base_dir: std::path::PathBuf::from("/tmp/web-coder"),
                source: crate::feat::skills::SkillSource::Global,
            }]);
        state.frontend.scope_stack.push(FocusScope::Picker {
            kind: PickerKind::Skill,
        });
        {
            let disabled = state.active_session().disabled_skills().clone();
            let theme = state.frontend.theme.clone();
            let discovered = state.active_session().discovered_skills().to_vec();
            reload_skill_picker_entries(&mut state.frontend, &discovered, &disabled, &theme);
        }
        state.frontend.skill_picker_mut().set_selection(0);
        assert!(state.frontend.caches.skill_preview_cache.is_empty());

        // When the picker is rendered twice at the same width.
        let area = Rect::new(0, 0, 100, 30);
        for _ in 0..2 {
            let backend = TestBackend::new(100, 30);
            let mut terminal = Terminal::new(backend).expect("terminal");
            terminal
                .draw(|frame| {
                    let slices = jinn_slices::Slices::new();
                    let overlay_views = crate::common::overlay_views::OverlayViews::new();
                    let ctx = RenderCtx::new(&state, &slices, &overlay_views)
                        .with_pickers(&crate::feat::picker::registry::build_picker_registry());
                    render_skill_picker(frame, area, &ctx);
                })
                .expect("draw");
        }

        // Then the cache holds exactly one entry; the second render was a cache hit.
        assert_eq!(
            state.frontend.caches.skill_preview_cache.len(),
            1,
            "second render should be a cache hit, not a second insert"
        );
    }

    /// Navigating from skill A -> B -> A should not re-render A on return; the
    /// cache should hold exactly {A, B} entries, proving A was a hit on the way back.
    #[rstest::rstest]
    #[test]
    fn render_skill_picker_switch_and_back_does_not_re_render_cached_skill() {
        // Given a picker with two skills, selection on the first.
        let mut state = AppState::default();
        state.active_session_mut().set_discovered_skills(vec![
            crate::feat::skills::Skill {
                name: "web-coder".to_owned(),
                description: "Web coder".to_owned(),
                body: "## Web body".to_owned(),
                file_path: std::path::PathBuf::from("/tmp/web-coder/SKILL.md"),
                base_dir: std::path::PathBuf::from("/tmp/web-coder"),
                source: crate::feat::skills::SkillSource::Global,
            },
            crate::feat::skills::Skill {
                name: "rust".to_owned(),
                description: "Rust".to_owned(),
                body: "## Rust body".to_owned(),
                file_path: std::path::PathBuf::from("/tmp/rust/SKILL.md"),
                base_dir: std::path::PathBuf::from("/tmp/rust"),
                source: crate::feat::skills::SkillSource::Global,
            },
        ]);
        state.frontend.scope_stack.push(FocusScope::Picker {
            kind: PickerKind::Skill,
        });
        {
            let disabled = state.active_session().disabled_skills().clone();
            let theme = state.frontend.theme.clone();
            let discovered = state.active_session().discovered_skills().to_vec();
            reload_skill_picker_entries(&mut state.frontend, &discovered, &disabled, &theme);
        }
        state.frontend.skill_picker_mut().set_selection(0);

        let draw = |state: &AppState, area: Rect| {
            let backend = TestBackend::new(area.width, area.height);
            let mut terminal = Terminal::new(backend).expect("terminal");
            terminal
                .draw(|frame| {
                    let slices = jinn_slices::Slices::new();
                    let overlay_views = crate::common::overlay_views::OverlayViews::new();
                    let ctx = RenderCtx::new(state, &slices, &overlay_views)
                        .with_pickers(&crate::feat::picker::registry::build_picker_registry());
                    render_skill_picker(frame, area, &ctx);
                })
                .expect("draw");
        };

        let area = Rect::new(0, 0, 100, 30);
        // Render skill A (web-coder) - populates cache with A.
        draw(&state, area);
        assert_eq!(state.frontend.caches.skill_preview_cache.len(), 1);

        // When navigating to skill B and rendering.
        state.frontend.skill_picker_mut().set_selection(1);
        draw(&state, area);
        assert_eq!(
            state.frontend.caches.skill_preview_cache.len(),
            2,
            "navigating to B should add a second entry"
        );

        // Then navigating back to A should NOT add a third entry (A is a cache hit).
        state.frontend.skill_picker_mut().set_selection(0);
        draw(&state, area);
        assert_eq!(
            state.frontend.caches.skill_preview_cache.len(),
            2,
            "returning to A should be a cache hit, not a re-render"
        );
    }

    /// Resizing the terminal (width change) re-renders for the new width; the cache
    /// holds two width-keyed entries for the same skill.
    #[rstest::rstest]
    #[test]
    fn render_skill_picker_width_change_creates_new_cache_entry() {
        // Given a picker with one skill.
        let mut state = AppState::default();
        state
            .active_session_mut()
            .set_discovered_skills(vec![crate::feat::skills::Skill {
                name: "web-coder".to_owned(),
                description: "Web coder".to_owned(),
                body: "## Body text".to_owned(),
                file_path: std::path::PathBuf::from("/tmp/web-coder/SKILL.md"),
                base_dir: std::path::PathBuf::from("/tmp/web-coder"),
                source: crate::feat::skills::SkillSource::Global,
            }]);
        state.frontend.scope_stack.push(FocusScope::Picker {
            kind: PickerKind::Skill,
        });
        {
            let disabled = state.active_session().disabled_skills().clone();
            let theme = state.frontend.theme.clone();
            let discovered = state.active_session().discovered_skills().to_vec();
            reload_skill_picker_entries(&mut state.frontend, &discovered, &disabled, &theme);
        }
        state.frontend.skill_picker_mut().set_selection(0);

        let draw = |state: &AppState, area: Rect| {
            let backend = TestBackend::new(area.width, area.height);
            let mut terminal = Terminal::new(backend).expect("terminal");
            terminal
                .draw(|frame| {
                    let slices = jinn_slices::Slices::new();
                    let overlay_views = crate::common::overlay_views::OverlayViews::new();
                    let ctx = RenderCtx::new(state, &slices, &overlay_views)
                        .with_pickers(&crate::feat::picker::registry::build_picker_registry());
                    render_skill_picker(frame, area, &ctx);
                })
                .expect("draw");
        };

        // When rendering at width 100 then width 140.
        // Both produce popups >= VERTICAL_SPLIT_MIN_WIDTH (101) only for 140;
        // the popup width differs (100-term => 80-col popup; 140-term => 112-col popup),
        // so the width keys must differ.
        draw(&state, Rect::new(0, 0, 100, 30));
        let width_100_entry_count = state.frontend.caches.skill_preview_cache.len();
        draw(&state, Rect::new(0, 0, 140, 30));

        // Then the cache holds two entries: one per width key.
        assert_eq!(
            width_100_entry_count, 1,
            "first render populates exactly one entry"
        );
        assert_eq!(
            state.frontend.caches.skill_preview_cache.len(),
            2,
            "width change should create a second width-keyed entry, not overwrite"
        );
    }

    #[rstest::rstest]
    #[test]
    fn render_project_picker_footer_documents_keybindings() {
        // Given a project picker with one entry.
        let mut state = AppState::default();
        {
            let theme = state.frontend.theme.clone();
            let entry = crate::feat::project::picker_entry::ProjectEntry::new(
                std::path::PathBuf::from("/tmp/project-a"),
                theme,
            );
            state.frontend.project_picker_mut().set_items(vec![entry]);
        }
        state.frontend.scope_stack.push(FocusScope::Picker {
            kind: PickerKind::Project,
        });

        // When rendering the project picker.
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| {
                let slices = jinn_slices::Slices::new();
                let overlay_views = crate::common::overlay_views::OverlayViews::new();
                let ctx = RenderCtx::new(&state, &slices, &overlay_views);
                let area = Rect::new(0, 0, 100, 30);
                render_project_picker(frame, area, &ctx);
            })
            .expect("draw");

        // Then the rendered footer documents the key bindings.
        let rendered: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(
            rendered.contains("<c-enter>"),
            "footer should advertise the <c-enter> new+lifecycle binding"
        );
        assert!(
            rendered.contains("<c-n>"),
            "footer should advertise the <c-n> add-dir binding"
        );
        assert!(
            rendered.contains("<c-d>"),
            "footer should advertise the <c-d> remove binding"
        );
        assert!(
            rendered.contains("remove"),
            "footer should advertise the remove action"
        );
        assert!(
            !rendered.contains("add cwd"),
            "footer should not advertise the removed a-add-cwd binding"
        );

        // And the keybind tokens are styled with accent_action (orange),
        // while the descriptive text uses muted_text (gray).
        let accent_action = state.frontend.theme.accent_action;
        let mut found_orange_keybind = false;
        for cell in &terminal.backend().buffer().content {
            if cell.fg == accent_action && !cell.symbol().trim().is_empty() {
                found_orange_keybind = true;
                break;
            }
        }
        assert!(
            found_orange_keybind,
            "at least one footer cell should use accent_action for a keybind"
        );
    }

    #[rstest::rstest]
    fn render_skill_picker_footer_advertises_tab_and_ctrl_l() {
        use crate::feat::skills::reload::reload_skill_picker_entries;

        // Given an open skill picker with one entry.
        let mut state = AppState::default();
        state
            .active_session_mut()
            .set_discovered_skills(vec![crate::feat::skills::Skill {
                name: "web-coder".to_owned(),
                description: "Web coder".to_owned(),
                body: "## Body text".to_owned(),
                file_path: std::path::PathBuf::from("/tmp/web-coder/SKILL.md"),
                base_dir: std::path::PathBuf::from("/tmp/web-coder"),
                source: crate::feat::skills::SkillSource::Global,
            }]);
        state.frontend.scope_stack.push(FocusScope::Picker {
            kind: PickerKind::Skill,
        });
        {
            let disabled = state.active_session().disabled_skills().clone();
            let theme = state.frontend.theme.clone();
            let discovered = state.active_session().discovered_skills().to_vec();
            reload_skill_picker_entries(&mut state.frontend, &discovered, &disabled, &theme);
        }

        // When rendering the skill picker.
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| {
                let slices = jinn_slices::Slices::new();
                let overlay_views = crate::common::overlay_views::OverlayViews::new();
                let ctx = RenderCtx::new(&state, &slices, &overlay_views)
                    .with_pickers(&crate::feat::picker::registry::build_picker_registry());
                let area = Rect::new(0, 0, 100, 30);
                render_skill_picker(frame, area, &ctx);
            })
            .expect("draw");

        // Then the rendered footer advertises the spec's bound keys.
        let rendered: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(
            rendered.contains("<tab>"),
            "footer should advertise the TAB toggle binding: {rendered}"
        );
        assert!(
            rendered.contains("<c-l>"),
            "footer should advertise the CTRL+L load binding"
        );
        assert!(
            rendered.contains("<c-r>"),
            "footer should still advertise the CTRL+R refresh binding"
        );

        // And at least one keybind token is styled with accent_action (orange).
        let accent_action = state.frontend.theme.accent_action;
        let found_orange_keybind = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .any(|c| c.fg == accent_action && !c.symbol().trim().is_empty());
        assert!(
            found_orange_keybind,
            "at least one footer cell should use accent_action for a keybind"
        );
    }
}
