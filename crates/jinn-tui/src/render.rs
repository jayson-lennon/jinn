//! Layout computation and rendering for the application.

pub mod app_layout;
pub mod chat_tab;
pub mod clipboard;
pub mod region_dispatch;
pub mod selection_highlight;
pub mod tab_bar;

pub mod too_small;
pub mod which_key;

pub use app_layout::{AppFrameLayout, AppLayout, MIN_HEIGHT, MIN_WIDTH, TabLayout};

use jinn_kernel::common::app_state::AppState;
use jinn_kernel::{FocusScope, Mode, RenderCtx};
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
    let ctx = RenderCtx::new(
        &state,
        &app.services.slices,
        &app.services.overlay_views,
        &app.services.config,
    );

    // Layout kind comes from the base scope's registration: a dynamic
    // tab scope renders full-width (no chat chrome); everything else is
    // the chat layout. The chat tab is the default for unregistered
    // scopes.
    let layout = AppFrameLayout::new(
        area,
        state.active_session().with_input(
            jinn_chat_input_msg::ChatInputBoxState::visual_line_count,
            || 0,
        ) as u16,
        area.height / 2,
        state.frontend.sidebar_width,
        is_full_width_tab(&app.services.slices, &state.frontend.scope_base()),
    );
    let active_scope = state.frontend.with_scope(
        |s| s.stack.current().clone(),
        || jinn_slices::FocusScope::Input,
    );
    let active_scope_ref = &active_scope;

    let mut rects = vec![];
    render_base_layers(
        &app.services.slices,
        &mut app.services.viewport,
        frame,
        &ctx,
        &layout,
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
///
/// The slices' own bookkeeping is not called here. Each slice that has
/// per-frame work to do registers a hook at activation; this pass takes
/// the write lock once and runs the registered hooks under it, then
/// sends whatever they asked to publish. The composition layer's own
/// work — the input's wrap width and cursor scroll — stays inline,
/// because the input box is a region it lays out rather than a slice
/// it reaches into.
fn apply_pre_render_mutation(app: &mut TuiApp, area: Rect) {
    let mut wstate = app.core.state.write();

    // Every picker measures its own results viewport in its render pass and
    // publishes it into its slice cell, so the kernel measures nothing.
    let full_width = is_full_width_tab(&app.services.slices, &wstate.frontend.scope_base());
    let pre_layout = AppFrameLayout::new(
        area,
        wstate.active_session().with_input(
            jinn_chat_input_msg::ChatInputBoxState::visual_line_count,
            || 0,
        ) as u16,
        area.height / 2,
        wstate.frontend.sidebar_width,
        full_width,
    );

    // The registered slice hooks, in the order their slices wired them.
    // The publish closures they return travel in the same order, so a
    // request and the deadline that bounds it stay paired.
    let hook_ctx = jinn_slices::PreRenderCtx {
        frame_area: area,
        chat: match &pre_layout {
            AppFrameLayout::Chat(chat) => Some(jinn_slices::ChatRects {
                main: chat.main,
                sidebar: chat.sidebar,
                input: chat.input,
            }),
            AppFrameLayout::Tab(_) => None,
        },
        config: &app.services.config,
    };
    for closure in app
        .services
        .slices
        .run_pre_render_hooks(&mut *wstate, &hook_ctx)
    {
        let _ = app.core.bridge.send(closure);
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
        }
    }
}
/// Renders the base layers for the active tab. In Chat mode: tab bar, border,
/// sidebar, chat tab, session/task-list previews, and status bar. In a
/// full-width dynamic tab: tab bar and the registered slice view only. The
/// which-key popup renders separately, after overlays — see the `render`
/// entry point.
fn render_base_layers(
    slices: &jinn_slices::Slices,
    viewport: &mut jinn_slices::view::Viewport,
    frame: &mut Frame<'_>,
    ctx: &RenderCtx<'_>,
    layout: &AppFrameLayout,
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
                let cx = jinn_slices::ViewCx {
                    theme: &ctx.state.frontend.theme,
                };
                viewport.render_slot(frame, dash.content, &slot, &cx, slices);
            }
        }
        AppFrameLayout::Chat(chat) => {
            tab_bar::render_tab_bar(frame, chat.tab_bar, ctx);
            chat_tab::border::render_border(frame, chat.border, ctx);
            // The dictation meter rides the border column's chat-log
            // stretch. It dispatches on every chat frame (the slice's
            // paint decides visible vs. idle-invisible from its cell);
            // the tab path below never reaches this arm, which is the
            // hidden-on-full-width-tabs rule.
            region_dispatch::draw_region(
                slices,
                jinn_slices::Region::DictationMeter,
                frame,
                jinn_slices::DrawTarget::new(chat.border),
                ctx,
                rects,
            );
            // The sidebar column: the slice's section list. It is
            // mouse-selectable only while it holds focus, which is what
            // `select` carries; the slice decides whether to register it,
            // the layout decides whether to offer it.
            let sidebar_select = ctx.state.frontend.is_sidebar().then_some(chat.sidebar);
            if let Some(draw) = slices.draw_for::<AppState>(jinn_slices::Region::Sidebar) {
                draw(
                    frame,
                    jinn_slices::DrawTarget::with_select(chat.sidebar, sidebar_select),
                    ctx,
                    rects,
                );
            }
            chat_tab::render_chat_tab(slices, frame, chat, ctx, rects);
            // The status bar is a slice-owned region too.
            region_dispatch::draw_region(
                slices,
                jinn_slices::Region::StatusBar,
                frame,
                jinn_slices::DrawTarget::new(chat.status_bar),
                ctx,
                rects,
            );
            // Surfaces that overflow the region anchoring them — the
            // sidebar's preview popups and confirmation banners reach left
            // across this layout. They come last so nothing they cover can
            // paint over them; the which-key popup below still comes after
            // these, above everything on screen.
            //
            // This is the layer's one dispatch. The order within it belongs
            // to the slice that registered it, and a second registrant brings
            // a `u16` priority per registration that the layer sorts by.
            region_dispatch::draw_region(
                slices,
                jinn_slices::Region::FloatingSurfaces,
                frame,
                jinn_slices::DrawTarget::new(chat.sidebar),
                ctx,
                rects,
            );
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
        // A `Picker` focus scope is a legacy name that no longer resolves —
        // every picker pushes a dynamic slice scope rendered below.
        // A saved scope predating the picker migration; no picker pushes it.
        #[expect(
            clippy::match_same_arms,
            reason = "the Input arm below has the same body by design; see the comment"
        )]
        FocusScope::Picker { .. } => None,
        FocusScope::Dynamic(id) => {
            // Slice overlays: consult the geometry fn + renderer the
            // scope's slice registered at activation. A dynamic scope
            // without either renders nothing. The rect is selectable only
            // when the slice opted in at activation.
            let overlay = ctx.slices.overlay(id)?;
            let overlay_area = overlay(&area)?;
            let view = ctx.overlay_view(id)?;
            let facts = ctx.facts();
            view(frame, overlay_area, &facts);
            ctx.slices.overlay_selectable(id).then_some(overlay_area)
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
