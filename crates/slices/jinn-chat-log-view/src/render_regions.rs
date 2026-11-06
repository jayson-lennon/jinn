//! The chat-log slice's region registrations.
//!
//! The chat layout asks "who draws the chat log, the minimap column,
//! and the audit popup?" and this module answers by registering the
//! slice's own draw functions at activation. Composition never names
//! a chat-log draw function; it resolves a [`Region`] and calls
//! whatever is registered for it.

use std::sync::Arc;

use jinn_core_types::{ChatEntry, ChatEntryKind};
use jinn_kernel::common::app_state::AppState;
use jinn_slices::{DrawContext, DrawTarget, Region};
use ratatui::Frame;
use ratatui::layout::Rect;

use crate::kernel_element::ChatLogElement;

/// Registers the chat-log regions against `slots`.
///
/// The element instance is created here and captured by the draw
/// closure, so there is exactly one per process and its throbber
/// animation state persists across frames.
pub fn register(slices: &jinn_slices::Slices) {
    // The registered draw function is a `Fn`, so it cannot hold a `&mut`
    // to the element across calls. The element is therefore held behind
    // interior mutability: one instance per process, so the loading
    // throbber's animation step persists across frames exactly as it
    // did when the element lived in the kernel's element registry.
    let element = parking_lot::Mutex::new(ChatLogElement::new());
    slices.register_render_slot::<AppState>(
        Region::ChatLog,
        Arc::new(
            move |frame: &mut Frame<'_>, target: DrawTarget, ctx, rects| {
                element.lock().paint(frame, target.area, ctx);
                // The chat log is selectable; whether the region is
                // *registered* as selectable depends on which column holds
                // focus, which the layout resolved into `target.select`.
                if let Some(select) = target.select {
                    rects.push(select);
                }
            },
        ),
    );

    // The vertical minimap column and the `>` arrow that points at the
    // selected entry. The arrow anchors to the chat log's rect, which
    // the layout carries alongside the minimap's own column.
    slices.register_render_slot::<AppState>(
        Region::Minimap,
        Arc::new(|frame: &mut Frame<'_>, target: DrawTarget, ctx, _rects| {
            let Some(chat_log_area) = target.select else {
                return;
            };
            minimap_region(frame, target.area, chat_log_area, ctx);
        }),
    );

    // The audit popup overlaying the chat log. Registered separately
    // because it paints *above* the log and its visibility is a
    // chat-log cell, not a layout decision.
    slices.register_render_slot::<AppState>(
        Region::AuditPopup,
        Arc::new(|frame: &mut Frame<'_>, target: DrawTarget, ctx, rects| {
            render_audit_popup(frame, target.area, ctx, rects);
        }),
    );
}

/// The vertical minimap column and the `>` arrow that points at the
/// selected entry.
///
/// The arrow is a second draw anchored to the chat log's rect rather
/// than to the minimap's own column, which is why the layout carries
/// the log rect alongside it. The indicator renders only in the
/// `Normal` focus scope: its counts overlay the log's right-edge text,
/// and drawing them while the input or an overlay holds focus would
/// obscure the text the user is looking at. The strip itself draws in
/// every scope.
pub fn minimap_region(
    frame: &mut Frame<'_>,
    minimap_area: Rect,
    chat_log_area: Rect,
    ctx: &dyn DrawContext<AppState>,
) {
    let state = ctx.state();
    let theme = &state.frontend.theme;

    let indicator_visible = matches!(state.frontend.scope(), jinn_slices::FocusScope::Normal);

    let arrow = crate::vertical_minimap::render_vertical_minimap(
        frame,
        minimap_area,
        state,
        theme.muted_text,
        ctx.config(),
    );

    if indicator_visible && let Some(ref arrow) = arrow {
        crate::minimap_arrow::render_minimap_arrow(frame, chat_log_area, arrow, theme.focus_accent);
    }
}

/// Renders the audit popup over the chat log, if it should be visible.
///
/// Visible when the slice's own audit-popup cell reads as visible, no
/// higher-priority overlay is active, and an entry is selected. The
/// "higher-priority overlay" test reads the focused scope's registered
/// render hint rather than matching a slice's scope name, so a new
/// modal scope declares its own suppression instead of being hard-coded
/// here.
pub fn render_audit_popup(
    frame: &mut Frame<'_>,
    chat_log_area: Rect,
    ctx: &dyn DrawContext<AppState>,
    rects: &mut Vec<Rect>,
) {
    if !crate::audit_popup::is_visible(ctx.slices()) {
        return;
    }
    if overlay_active(ctx) {
        return;
    }
    let Some(entry) = ctx.state().active_session().selected_entry() else {
        return;
    };
    let model = resolve_audit_model(entry, ctx.state().active_session().history());
    let lines = crate::chat_log::format_audit_lines(entry, &ctx.state().frontend.theme, model);

    // Resolves to `None` until the render pipeline has populated the
    // cached fields for this frame; the popup then simply skips a frame.
    let Some(entry_top_y) = ctx
        .state()
        .active_session()
        .selected_entry_screen_y(chat_log_area.y)
    else {
        return;
    };
    let rect = crate::chat_log::audit_popup_rect(chat_log_area, entry_top_y, lines.len());

    // Clear the underlying buffer so the popup is opaque.
    frame.render_widget(ratatui::widgets::Clear, rect);

    let block = ratatui::widgets::Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .style(
            ratatui::style::Style::default()
                .bg(ctx.state().frontend.theme.infopopup_bg)
                .fg(ctx.state().frontend.theme.infopopup_border),
        );
    let paragraph = ratatui::widgets::Paragraph::new(lines).style(
        ratatui::style::Style::default()
            .bg(ctx.state().frontend.theme.infopopup_bg)
            .fg(ctx.state().frontend.theme.infopopup_fg),
    );
    frame.render_widget(paragraph.block(block), rect);

    rects.push(rect);
}

/// Resolves the `(used, requested)` model pair the audit popup shows for
/// `entry`.
///
/// Most entries answer from their own stamps. A tool result is the one
/// exception — it is produced locally by the tool executor after the stream
/// ended and carries no model of its own — so its model is its paired tool
/// call's: "which model made this call" is the question being asked, and the
/// call is the artifact the model actually produced. An unpaired result (the
/// call's entry gone) answers nothing rather than guessing.
fn resolve_audit_model<'a>(
    entry: &'a ChatEntry,
    history: &'a [ChatEntry],
) -> Option<(&'a str, Option<&'a str>)> {
    if let ChatEntryKind::ToolResult { id, .. } = &entry.kind {
        let call = history.iter().find(|candidate| {
            matches!(
                &candidate.kind,
                ChatEntryKind::ToolCall {
                    id: call_id,
                    ..
                } if call_id == id
            )
        })?;
        return Some((call.model_used.as_deref()?, call.model_requested.as_deref()));
    }
    let used = entry.model_used.as_deref()?;
    Some((used, entry.model_requested.as_deref()))
}

/// Returns true when a higher-priority overlay is currently active.
fn overlay_active(ctx: &dyn DrawContext<AppState>) -> bool {
    match ctx.state().frontend.scope() {
        jinn_slices::FocusScope::Picker { .. } => true,
        jinn_slices::FocusScope::Dynamic(id) => ctx.slices().hint_for(&id).suppresses_lower_overlay,
        _ => false,
    }
}

#[cfg(test)]
mod audit_model_tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing, reason = "test code")]
    //! Tests for `resolve_audit_model`: the entry's own pair, the tool
    //! result's derivation from its paired call, and the no-model cases.

    use super::*;
    use jinn_core_types::ToolCallId;
    use jinn_core_types::ToolResultStatus;

    #[rstest::rstest]
    #[test]
    fn entry_uses_its_own_model_pair() {
        // Given an assistant entry stamped with both models.
        let mut entry = ChatEntry::assistant("answer");
        entry.restore_model_used(Some("openai/gpt-5.1".to_owned()));
        entry.restore_model_requested(Some("openrouter/auto".to_owned()));

        // When resolving its audit model.
        let model = resolve_audit_model(&entry, std::slice::from_ref(&entry));

        // Then the pair is the entry's own.
        assert_eq!(model, Some(("openai/gpt-5.1", Some("openrouter/auto"))));
    }

    #[rstest::rstest]
    #[test]
    fn popup_shows_model_for_tool_result_via_call() {
        // Given a history holding a stamped tool call and its result.
        let mut call = ChatEntry::tool_call(ToolCallId::from("tc-1"), "read", "{}");
        call.restore_model_used(Some("anthropic/claude-sonnet-4.5".to_owned()));
        call.restore_model_requested(Some("openrouter/auto".to_owned()));
        let result = ChatEntry::tool_result(
            ToolCallId::from("tc-1"),
            "read",
            "contents",
            ToolResultStatus::Success,
        );
        let history = vec![call, result];

        // When resolving the result's audit model.
        let model = resolve_audit_model(&history[1], &history);

        // Then it is the paired call's pair, not the result's (unset) one.
        assert_eq!(
            model,
            Some(("anthropic/claude-sonnet-4.5", Some("openrouter/auto")))
        );
    }

    #[rstest::rstest]
    #[test]
    fn unpaired_tool_result_resolves_no_model() {
        // Given a result whose call entry is gone from history.
        let result = ChatEntry::tool_result(
            ToolCallId::from("tc-gone"),
            "read",
            "contents",
            ToolResultStatus::Success,
        );

        // When resolving its audit model.
        let model = resolve_audit_model(&result, &[]);

        // Then nothing is offered rather than a guess.
        assert_eq!(model, None);
    }

    #[rstest::rstest]
    #[test]
    fn unstamped_entry_resolves_no_model() {
        // Given an entry created before attribution existed.
        let entry = ChatEntry::user("hi");

        // When resolving its audit model.
        let model = resolve_audit_model(&entry, std::slice::from_ref(&entry));

        // Then there is no pair to show.
        assert_eq!(model, None);
    }
}
