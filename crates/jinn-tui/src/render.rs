//! Layout computation and rendering for the application.

pub mod app_layout;
pub mod chat_tab;
pub mod clipboard;
pub mod picker;
pub mod selection_highlight;
pub mod status_bar;
pub mod tab_bar;
pub mod terminal_tab;

pub mod too_small;
pub mod which_key;

pub use app_layout::{AppFrameLayout, AppLayout, MIN_HEIGHT, MIN_WIDTH, TabLayout};

use jinn_domain::{AppUiRegistry, FocusScope, Mode, RenderCtx, feat::ui::picker_states::PickerExt};
use jinn_sidebar::sections::Sidebar;
use ratatui::{Frame, layout::Rect};

use crate::TuiApp;

/// Renders the full application frame.
pub fn render(app: &mut TuiApp, frame: &mut Frame<'_>) {
    let area = frame.area();
    if !AppLayout::meets_min_size(area) {
        too_small::render_too_small(frame, area, app);
        return;
    }

    apply_pre_render_mutation(app, area);

    let state = app.core.state.read();
    let ctx = RenderCtx::new(&state, &app.services.slices, &app.services.overlay_views)
        .with_pickers(&app.services.picker_registry);

    // Layout kind comes from the base scope's registration: a dynamic
    // tab scope renders full-width (no chat chrome); everything else is
    // the chat layout. The chat tab is the default for unregistered
    // scopes.
    let layout = AppFrameLayout::new(
        area,
        state
            .active_session()
            .with_input(jinn_slices::ChatInputBoxState::visual_line_count, || 0) as u16,
        area.height / 2,
        state.frontend.sidebar_width,
        is_full_width_tab(&app.services.slices, &state.frontend.scope_base()),
    );
    let sidebar_focused = state.frontend.is_sidebar();
    let active_scope = state.frontend.with_scope(
        |s| s.stack.current().clone(),
        || jinn_domain::FocusScope::Input,
    );
    let active_scope_ref = &active_scope;

    let mut rects = vec![];
    render_base_layers(
        &app.services.slices,
        &mut app.services.viewport,
        &mut app.sidebar,
        &mut app.ui_registry,
        frame,
        &ctx,
        &layout,
        area,
        sidebar_focused,
        &mut rects,
    );
    if let Some(rect) = render_active_overlay(frame, area, &ctx, active_scope_ref) {
        rects.push(rect);
    }
    // The which-key help popup paints last so it sits above every overlay
    // (e.g. the terminal overlay would otherwise obscure it in view mode).
    which_key::render_which_key(frame, &mut app.which_key, &ctx);

    drop(state);

    app.selectable_rects.rebuild(rects);
    selection_highlight::apply_selection_highlight(app, frame.buffer_mut());
    clipboard::flush_pending_clipboard(app, frame.buffer_mut());
}

/// Sets wrap width and scroll offset before layout, using a write lock.
fn apply_pre_render_mutation(app: &mut TuiApp, area: Rect) {
    let mut wstate = app.core.state.write(&app.intent_handler_cap);

    // Measure the active picker's results viewport every frame so navigation
    // intents scroll against the real on-screen height instead of a stale
    // hardcoded constant.
    let picker_viewport = jinn_domain::feat::picker::geometry::measure_active_picker_results_height(
        &wstate,
        area,
        &app.services.picker_registry,
    );
    wstate.frontend.set_picker_results_viewport(picker_viewport);
    let full_width = is_full_width_tab(&app.services.slices, &wstate.frontend.scope_base());
    let pre_layout = AppFrameLayout::new(
        area,
        wstate
            .active_session()
            .with_input(jinn_slices::ChatInputBoxState::visual_line_count, || 0) as u16,
        area.height / 2,
        wstate.frontend.sidebar_width,
        full_width,
    );
    // The terminal overlay's inner rect sizes the pty (WYSIWYG). Computed
    // every frame while open; deduped by the mirror, sent through the bridge.
    if matches!(
        wstate.frontend.scope(),
        jinn_domain::FocusScope::TerminalView | jinn_domain::FocusScope::TerminalControl
    ) {
        let inner =
            jinn_domain::feat::interactive_term::overlay_geometry::terminal_overlay_inner_rect(
                area,
            );
        let (rows, cols) = (inner.height, inner.width);
        if wstate.frontend.terminal.record_layout_size(rows, cols) {
            let closure = jinn_domain::common::bridge::Bridge::publish_closure(
                jinn_domain::feat::interactive_term::protocol::command::ResizeTerm {
                    chat_session_id: Some(wstate.session.active_session_id().clone()),
                    size: (rows, cols),
                },
            );
            let _ = app.core.bridge.send(closure);
        }
    }
    match &pre_layout {
        // The dashboard slice lives outside AppState; its scroll clamp is
        // the actor's concern (ratatui re-derives visibility per frame).
        AppFrameLayout::Tab(_) => {}
        AppFrameLayout::Chat(chat) => {
            let text_width = chat.main.width.saturating_sub(2) as usize;
            wstate
                .active_session()
                .update_input(|i| i.set_wrap_width(text_width));
            if wstate.frontend.scope().mode() == Mode::Input {
                let inner_height = chat.input.height.saturating_sub(1) as usize;
                wstate
                    .active_session()
                    .update_input(|i| i.scroll_to_cursor(inner_height));
            }
            jinn_sidebar::sections::task_list_section::preview::write_preview_geometry(
                &mut wstate,
                area,
                chat.sidebar,
            );
        }
    }

    refresh_mcp_inspector_snapshot(&mut wstate);
}

/// Refreshes the selected MCP server picker entry's live status/stderr/tools
/// snapshot from the active session's maps before render reads it. No-op
/// unless the MCP server inspector is the active overlay.
fn refresh_mcp_inspector_snapshot(state: &mut jinn_domain::AppState) {
    use jinn_domain::FocusScope;
    let is_mcp_picker = matches!(
        &state.frontend.scope(),
        FocusScope::Picker {
            kind: jinn_domain::PickerKind::McpServer
        },
    );
    if !is_mcp_picker {
        return;
    }
    let server_name = match state.frontend.mcp_server_picker().selected_item() {
        Some(item) => item.entry().name.clone(),
        None => return,
    };
    let session_id = state.active_session().session_id().clone();
    let (status, stderr_tail, tools) = {
        let session = state.active_session();
        let status = session.mcp_server_status().get(&server_name).copied();
        let stderr_tail = session
            .mcp_server_stderr()
            .get(&server_name)
            .cloned()
            .unwrap_or_default();
        let defs = state.context.tools_for_session(&session_id);
        jinn_domain::feat::mcp::picker_entry::refresh_snapshot(
            &server_name,
            status,
            &stderr_tail,
            &defs,
        )
    };
    state
        .frontend
        .mcp_server_picker_mut()
        .with_selected_mut(|item| {
            let entry = item.entry_mut();
            entry.status = status;
            entry.stderr_tail = stderr_tail;
            entry.tools = tools;
        });
}

/// Renders the base layers for the active tab. In Chat mode: tab bar, border,
/// sidebar, chat tab, session/task-list previews, and status bar. In a
/// full-width dynamic tab: tab bar and the registered slice view only. The
/// which-key popup renders separately, after overlays — see the `render`
/// entry point.
#[expect(
    clippy::too_many_arguments,
    reason = "all inputs are single-use render pass params"
)]
fn render_base_layers(
    slices: &jinn_domain::common::slices::Slices,
    viewport: &mut jinn_domain::common::slices::view::Viewport,
    sidebar: &mut Sidebar,
    ui_registry: &mut AppUiRegistry,
    frame: &mut Frame<'_>,
    ctx: &RenderCtx<'_>,
    layout: &AppFrameLayout,
    frame_area: Rect,
    sidebar_focused: bool,
    rects: &mut Vec<Rect>,
) {
    match layout {
        AppFrameLayout::Tab(dash) => {
            tab_bar::render_tab_bar(frame, dash.tab_bar, ctx);
            // The active tab's slice view draws the content: the base
            // scope's slot resolves through the viewport. An unregistered
            // slot renders nothing (blank tab — a wiring bug caught by
            // the startup pairing check, not silently here).
            let base = ctx.state.frontend.scope_base();
            if let FocusScope::Dynamic(ref id) = base
                && let Some(slot) = slices.tab_slot(id)
            {
                let cx = jinn_domain::common::slices::ViewCx {
                    theme: &ctx.state.frontend.theme,
                };
                viewport.render_slot(frame, dash.content, &slot, &cx, slices);
            }
        }
        AppFrameLayout::Chat(chat) => {
            tab_bar::render_tab_bar(frame, chat.tab_bar, ctx);
            chat_tab::border::render_border(frame, chat.border, ctx);
            chat_tab::sidebar::render_sidebar(
                sidebar,
                frame,
                chat.sidebar,
                sidebar_focused,
                ctx,
                rects,
            );
            chat_tab::render_chat_tab(ui_registry, frame, chat, ctx, rects);
            jinn_sidebar::sections::sessions::render_archive_tree_prompt_for_state(
                frame,
                chat.sidebar,
                frame_area,
                ctx,
            );
            jinn_sidebar::sections::sessions::render_close_session_prompt_for_state(
                frame,
                chat.sidebar,
                frame_area,
                ctx,
            );
            jinn_sidebar::sections::sessions::render_session_preview_for_state(
                frame,
                chat.sidebar,
                frame_area,
                ctx,
            );
            jinn_sidebar::sections::task_list_section::preview::render_task_list_preview_for_state(
                frame,
                chat.sidebar,
                frame_area,
                ctx,
            );
            status_bar::render_status_bar(ui_registry, frame, chat.status_bar, ctx);
        }
    }
}

/// Renders the single popup matching the active scope, if any, and returns its
/// selectable rect so the caller can register it.
fn render_active_overlay(
    frame: &mut Frame<'_>,
    area: Rect,
    ctx: &RenderCtx<'_>,
    scope: &FocusScope,
) -> Option<Rect> {
    match scope {
        FocusScope::Picker { .. } => {
            picker::render_picker(frame, area, ctx);
            Some(jinn_selection_widget::compute_popup_rect(area))
        }
        FocusScope::ArgInput => {
            picker::render_arg_input(frame, area, ctx);
            Some(jinn_domain::feat::session_lifecycle::render::arg_input_popup_rect(area, ctx))
        }
        FocusScope::RenameSessionInput => {
            jinn_sidebar::sections::rename_input::render::render_rename_session_input(
                frame, area, ctx,
            );
            Some(jinn_sidebar::sections::rename_input::render::rename_session_popup_rect(area))
        }
        FocusScope::PrunerAccumulationInput => {
            jinn_domain::feat::pruner_accumulation_input::render::render_pruner_accumulation_input(
                frame, area, ctx,
            );
            Some(jinn_domain::feat::pruner_accumulation_input::render::pruner_accumulation_popup_rect(area))
        }
        FocusScope::CwdInput => {
            jinn_domain::feat::cwd_input::render::render_cwd_input(frame, area, ctx);
            Some(jinn_domain::feat::cwd_input::render::cwd_input_popup_rect(
                area,
            ))
        }
        FocusScope::ProjectAddInput => {
            jinn_domain::feat::project_add_input::render::render_project_add_input(
                frame, area, ctx,
            );
            Some(jinn_domain::feat::project_add_input::render::project_add_input_popup_rect(area))
        }
        FocusScope::TerminalView | FocusScope::TerminalControl => {
            let overlay_rect =
                jinn_domain::feat::interactive_term::overlay_geometry::terminal_overlay_rect(area);
            crate::render::terminal_tab::render_terminal_tab(frame, overlay_rect, ctx);
            Some(overlay_rect)
        }
        FocusScope::Dynamic(id) => {
            // Slice overlays: consult the geometry fn + renderer the
            // scope's slice registered at activation. A dynamic scope
            // without either renders nothing.
            let overlay = ctx.slices.overlay(id)?;
            let overlay_area = overlay(&area)?;
            let view = ctx.overlay_view(id)?;
            let facts = ctx.facts();
            view(frame, overlay_area, &facts);
            None
        }
        _ => None,
    }
}

/// Returns `true` when `scope` is a registered full-width tab.
///
/// Tab scopes are declared by slices at activation; the chat tab is
/// the fallback for Normal and any unregistered scope.
fn is_full_width_tab(slices: &jinn_slices::Slices, scope: &FocusScope) -> bool {
    match scope {
        FocusScope::Dynamic(id) => slices.tab_scopes().contains(id),
        _ => false,
    }
}
