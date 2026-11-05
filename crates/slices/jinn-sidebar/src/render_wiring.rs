//! The sidebar's slice-owned rendering registrations.
//!
//! The chat layout draws a border between the main column and the
//! sidebar, and a bottom line under the content. Both lines change
//! colour with whatever owns the focused scope — the sidebar claims
//! the focused accent, the sidebar's *resize* mode claims its own
//! transient accent, and every other scope leaves the chrome unfocused.
//!
//! Those are the sidebar's own rules, so the sidebar states them here,
//! once, at activation. The chrome reads a registered hint instead of
//! matching the string `"sidebar"` against a scope id.
//!
//! The five surfaces that overflow the column — the archive-tree prompt,
//! the close-session prompt, the session-ID yank banner, the session
//! preview, and the task-list preview — register separately, against
//! [`jinn_slices::Region::FloatingSurfaces`]. They reach left across the
//! chat column, and the render pass draws the chat column *after* the
//! sidebar, so painting them from the column's own draw call would put
//! them underneath it. Their own layer is what makes the column's
//! contents and the surfaces that overflow it two different draws.

use std::sync::Arc;

use jinn_kernel::common::app_state::AppState;
use jinn_slices::DrawContext;
use jinn_slices::slice_scope::SliceScopeId;
use jinn_slices::{ChatRects, PreRenderCtx, ScopeRenderHint};
use ratatui::Frame;
use ratatui::layout::Rect;

use crate::sections::section_trait::SidebarSectionId;

/// Registers the render hints for every scope the sidebar owns.
///
/// Two rules, stated as data:
///
/// * Each section's navigation scope claims the chrome's *focused*
///   accent, because the sidebar is the focus owner while one of them
///   is active.
/// * The resize scope claims the *acting* accent, which is the
///   transient colour a drag uses. It is a different visual state from
///   merely being focused, and the sidebar is the only slice that
///   knows that.
/// * The rename popup's scope stands a lower overlay down, because
///   the rename input is modal over the sessions list and the chat
///   log's audit popup must not paint through it.
pub fn register_hints(slices: &jinn_slices::Slices) {
    for section in [
        SidebarSectionId::Pins,
        SidebarSectionId::Persona,
        SidebarSectionId::Bot,
        SidebarSectionId::TaskList,
        SidebarSectionId::McpServers,
        SidebarSectionId::Library,
    ] {
        slices.register_scope_hint(section.scope_id(), ScopeRenderHint::focused());
    }
    slices.register_scope_hint(resize_scope(), ScopeRenderHint::acting());
    // Two scopes stand a lower overlay down, and they stand it for
    // different reasons, so each claims it rather than the chrome
    // listing sidebar scope names.
    slices.register_scope_hint(
        SidebarSectionId::Sessions.scope_id(),
        // The sessions list owns a rename input that paints over the
        // chat log, so the chat log's audit popup yields to it.
        ScopeRenderHint::focused().suppressing_lower_overlay(),
    );
    slices.register_scope_hint(
        rename_scope(),
        // The rename popup is modal over the sessions list.
        ScopeRenderHint::focused().suppressing_lower_overlay(),
    );
}

/// The sidebar's resize-mode scope.
fn resize_scope() -> SliceScopeId {
    jinn_sidebar_msg::SidebarSectionId::resize_scope_id()
}

/// The rename popup's scope.
fn rename_scope() -> SliceScopeId {
    crate::key_routes::rename_scope()
}

/// The sidebar's per-frame write hooks, in the order the render pass
/// runs them.
///
/// The pre-render pass holds one write lock and drives these in
/// sequence, so the order is part of the contract: the cursor rows
/// are recorded before anything reads them, the preview width is
/// recorded before anything reads it, and the scroll offset is written
/// after the geometry that depends on it.
pub fn register_pre_render_hooks(slices: &jinn_slices::Slices) {
    slices.push_pre_render_hook::<AppState>(Arc::new(record_preview_width));
    slices.push_pre_render_hook::<AppState>(Arc::new(request_session_preview));
    slices.push_pre_render_hook::<AppState>(Arc::new(record_cursor_rows));
    slices.push_pre_render_hook::<AppState>(Arc::new(write_task_list_geometry));
    slices.push_pre_render_hook::<AppState>(Arc::new(write_scroll_offset));
}

/// Records which row each sidebar cursor is sitting on.
///
/// Unconditional, like [`record_preview_width`] and unlike the two hooks below
/// it: a cursor's row is a fact about state, not about the current frame's
/// geometry. A frame drawn in a full-width tab has no sidebar column at all, and
/// a frame drawn while the chat pane holds focus still knows where the sidebar's
/// cursors are — gating either of those away is how the record goes stale
/// exactly when a removal needs it.
///
/// Runs before the scroll offset is written, which is derived from the cursor
/// row the frame is drawing: the two must describe the same frame.
fn record_cursor_rows(
    state: &mut AppState,
    _ctx: &PreRenderCtx<'_>,
) -> Vec<jinn_slices::route::PublishClosure> {
    crate::sections::capture_rows::capture_sessions_cursor_row(state);
    crate::sections::capture_rows::capture_bot_cursor_row(state);
    crate::sections::capture_rows::capture_library_cursor_row(state);
    Vec::new()
}

/// Records the width a session preview wraps its lines at.
///
/// The preview's render pass and the keyboard path must agree on the
/// width or the rendered lines can never match the lookup and the
/// preview spins forever. It is recorded here, in the one pass that
/// runs every frame with the true area and no early return —
/// recording it from the preview's own draw would leave the window
/// between a cursor move and the next frame asking for a stale width.
fn record_preview_width(
    state: &mut AppState,
    ctx: &PreRenderCtx<'_>,
) -> Vec<jinn_slices::route::PublishClosure> {
    let width = crate::sections::sessions::preview::preview_content_width(ctx.frame_area);
    state
        .frontend
        .update_sections(|s| s.sessions.preview_content_width = width);
    Vec::new()
}

/// Requests a session preview when the cache is cold and nothing is in
/// flight.
///
/// The popup's cache is not driven by the cursor alone: a session can
/// load into the list, or the width can be measured, long after the
/// last key. A cache miss with nothing in flight is a request nobody
/// made, so this pass makes it. `update_preview` dedupes against the
/// cache and against in-flight renders, so a settled cursor publishes
/// once and then stays silent.
fn request_session_preview(
    state: &mut AppState,
    ctx: &PreRenderCtx<'_>,
) -> Vec<jinn_slices::route::PublishClosure> {
    let Some(request) =
        crate::sections::sessions::preview_load::request_preview_if_needed(state, ctx.config)
    else {
        return Vec::new();
    };
    // The request and its deadline travel together, exactly as the
    // keyboard path sends them: a render nobody watches is a spinner
    // with nothing to end it.
    crate::sections::sessions::navigate::preview_messages(request).messages
}

/// Measures the task-list preview popup and writes its geometry into
/// state, so the scroll intents can page by a full viewport and clamp
/// without re-wrapping. A tab layout has no sidebar column, so the
/// hook stands down.
fn write_task_list_geometry(
    state: &mut AppState,
    ctx: &PreRenderCtx<'_>,
) -> Vec<jinn_slices::route::PublishClosure> {
    let Some(ChatRects { sidebar, .. }) = ctx.chat else {
        return Vec::new();
    };
    crate::sections::task_list_section::preview::write_preview_geometry(
        state,
        ctx.config,
        ctx.frame_area,
        sidebar,
    );
    Vec::new()
}

/// Records the sidebar's scroll offset for the frame about to render.
///
/// The offset is a pure function of the cursor, so there is nothing to
/// recompute here — this only remembers it, which is what lets the
/// column hold its position across a focus change to the chat pane
/// (where no cursor can be derived at all).
fn write_scroll_offset(
    state: &mut AppState,
    ctx: &PreRenderCtx<'_>,
) -> Vec<jinn_slices::route::PublishClosure> {
    let Some(ChatRects { sidebar, .. }) = ctx.chat else {
        return Vec::new();
    };
    crate::sections::layout::write_scroll_offset(state, ctx.config, sidebar.height);
    Vec::new()
}

/// Builds the draw function the render pass calls for the sidebar column.
///
/// The column is the section list, and laying a section out mutates its
/// own cursor and cache state, so drawing needs a `&mut Sidebar`. A
/// registered draw function is a `Fn` and cannot hold that borrow across
/// frames, so the container sits behind interior mutability: one process
/// has one sidebar, so one instance is the right cardinality, and each
/// section's state persists frame to frame exactly as it did when the
/// container lived on `TuiApp`.
///
/// The column paints the column and nothing else. The surfaces that
/// overflow it are a separate draw function, registered against
/// [`jinn_slices::Region::FloatingSurfaces`], because a popup that reaches
/// left across the chat column has to be painted after the chat column —
/// and this call happens before it.
#[must_use]
pub fn column_draw_fn() -> jinn_slices::DrawFn<AppState> {
    let sidebar = parking_lot::Mutex::new({
        let mut sidebar = crate::sections::sidebar::Sidebar::new();
        crate::sections::register_sections(&mut sidebar);
        sidebar
    });
    Arc::new(
        move |frame: &mut Frame<'_>,
              target: jinn_slices::DrawTarget,
              ctx: &dyn DrawContext<AppState>,
              rects: &mut Vec<Rect>| {
            let rect = target.area;
            sidebar.lock().render(frame, rect, ctx);
            if let Some(select) = target.select {
                rects.push(select);
            }
        },
    )
}

/// Builds the draw function for the sidebar's floating surfaces.
///
/// The layer exists so a surface wider than the sidebar can be painted
/// after the columns it covers rather than under them. Each surface decides
/// for itself whether it has anything to draw — a banner with no prompt
/// pending and a preview with no cache entry both paint nothing — so the
/// sequence below is unconditional and costs five cheap checks a frame.
///
/// The order is the stacking order, bottom first: a banner is a narrower
/// strip that a preview, sitting under the user's cursor, may cover, and
/// the task-list preview is the topmost surface the sidebar owns. When a
/// second slice registers against [`jinn_slices::Region::FloatingSurfaces`],
/// the order moves out of this sequence and onto the registrations, each
/// carrying a `u16` priority that the layer sorts by with ties broken by
/// call order.
#[must_use]
pub fn floating_surfaces_draw_fn() -> jinn_slices::DrawFn<AppState> {
    Arc::new(
        move |frame: &mut Frame<'_>,
              target: jinn_slices::DrawTarget,
              ctx: &dyn DrawContext<AppState>,
              _rects: &mut Vec<Rect>| {
            draw_floating_surfaces(frame, target.area, frame.area(), ctx);
        },
    )
}

/// Paints the sidebar's floating surfaces, bottom of the stack first.
///
/// Anchored to the sidebar's own rect, so a surface may extend left across
/// the chat column, the input box, and the status bar. The order is the
/// stacking order; see [`floating_surfaces_draw_fn`].
pub fn draw_floating_surfaces(
    frame: &mut Frame<'_>,
    sidebar_rect: Rect,
    frame_area: Rect,
    ctx: &dyn DrawContext<AppState>,
) {
    crate::sections::sessions::render_archive_tree_prompt_for_state(
        frame,
        sidebar_rect,
        frame_area,
        ctx,
    );
    crate::sections::sessions::render_close_session_prompt_for_state(
        frame,
        sidebar_rect,
        frame_area,
        ctx,
    );
    crate::sections::sessions::render_session_preview_for_state(
        frame,
        sidebar_rect,
        frame_area,
        ctx,
    );
    crate::sections::bots_reports::render_bot_reports_for_state(
        frame,
        sidebar_rect,
        frame_area,
        ctx,
    );
    crate::sections::task_list_section::preview::render_task_list_preview_for_state(
        frame,
        sidebar_rect,
        frame_area,
        ctx,
    );
}
