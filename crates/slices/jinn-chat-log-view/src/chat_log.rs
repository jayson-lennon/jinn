//! Chat log rendering — the pure pipeline that turns chat entries into lines.
//!
//! Each entry kind has its own adapter that produces the styled `Line`s for
//! that entry: the user block, the system note, the tool call, the tool
//! result, the annotation, and so on. [`entry_to_lines`] dispatches an entry
//! to the right adapter, and [`RenderContext`] carries the per-frame theme
//! and width the adapters need.
//!
//! Nothing here reads application state. The adapters are pure functions of
//! their arguments and the context, which is what lets them live in this
//! slice rather than in the kernel: the kernel's `ChatLogElement` is a thin
//! `UiElement` wrapper that reads `AppState` and hands the resolved inputs
//! to [`entry_to_lines`].

pub(crate) mod actor;
pub(crate) mod annotation;
pub(crate) mod assistant;
pub mod audit_popup;
pub(crate) mod compaction;
pub(crate) mod error_entry;
pub mod gutter;
pub(crate) mod intersession;
pub(crate) mod markdown;
pub(crate) mod rule_interrupt;
pub mod scroll_indicator;
pub(crate) mod shared;

pub(crate) mod system;
pub(crate) mod thinking;
pub(crate) mod tool_call;
pub(crate) mod tool_result;
pub(crate) mod transient;
pub(crate) mod user;
pub mod viewport;

pub use audit_popup::format_audit_lines;
pub use audit_popup::{AUDIT_POPUP_WIDTH, audit_popup_rect};
pub use gutter::{
    GutterStyle, build_blank_gutter_lines, build_collapsed_block_gutter_line,
    build_entry_gutter_lines,
};

pub use markdown::render_markdown;
pub use scroll_indicator::render_scroll_indicator;
pub use shared::{EntryGraphics, GUTTER_WIDTH, RenderContext, strip_ansi};
pub use viewport::{ScrollState, compute_scroll, find_visible_indices};

use jinn_chat_log_view_msg::{ImageKey, ImageSlot};
use jinn_core_types::attachment::Attachment;
use jinn_core_types::{ChatEntry, ChatEntryKind};
use jinn_tools_msg::truncation::format_size;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Wrap};

/// Emits the blank slot lines for one image attachment and records the slot.
///
/// Slot lines are one fully-padded row each — a single space span stretched
/// to `ctx.content_width` carrying the block background, when one is given.
/// They wrap to exactly one row at any width, which is what keeps the
/// wrapped count equal to `lines.len()` and the line index stable.
/// `rows == 0` (no slot for this attachment) emits nothing and returns the
/// untouched line count.
fn emit_slot_lines(
    lines: &mut Vec<Line<'static>>,
    key: ImageKey,
    rows: u16,
    block_bg: Option<ratatui::style::Color>,
    ctx: &RenderContext,
    slots_out: Option<&mut Vec<ImageSlot>>,
) {
    if rows == 0 {
        return;
    }
    let line_index = u32::try_from(lines.len()).unwrap_or(u32::MAX);
    let bg_style = block_bg.map(|bg| Style::default().bg(bg));
    for _ in 0..rows {
        let mut line = Line::from(String::new());
        if let Some(style) = bg_style {
            shared::pad_line_to_width(&mut line, ctx.content_width, style);
        }
        lines.push(line);
    }
    if let Some(out) = slots_out {
        out.push(ImageSlot {
            key,
            line_index,
            rows,
        });
    }
}

/// Converts each slot's logical line index to the wrapped row offset the
/// paint pass needs.
///
/// Slot lines are exactly one wrapped row each, but the entry's content
/// above them may wrap to more rows than its logical count, so the offset
/// is measured the same way [`wrapped_line_count`] measures the entry:
/// `Paragraph::line_count` over the lines before the slot. Called by the
/// render pass right after rendering, while the lines are in hand — the
/// cache-hit path converts from its cached lines identically.
pub fn wrapped_slot_offsets(lines: &[Line<'static>], slots: &mut [ImageSlot], content_width: u16) {
    for slot in slots {
        let before = lines.get(..slot.line_index as usize).unwrap_or(&[]);
        let wrapped_before = if content_width == 0 {
            before.len() as u32
        } else {
            Paragraph::new(before.to_vec())
                .wrap(Wrap { trim: false })
                .line_count(content_width) as u32
        };
        slot.line_index = wrapped_before;
    }
}

/// One-line placeholder summary for a tool result's image attachments
/// (`🖼 shot.png — image/png, 2.0KB`), or `None` when there are none.
/// Carries names and sizes only — never bytes.
fn attachments_summary(attachments: &[Attachment]) -> Option<String> {
    let images: Vec<&Attachment> = attachments.iter().filter(|a| a.is_image()).collect();
    if images.is_empty() {
        return None;
    }
    let parts: Vec<String> = images
        .iter()
        .map(|a| {
            format!(
                "🖼 image — {}, {}",
                a.media_type(),
                format_size(a.data().len())
            )
        })
        .collect();
    Some(parts.join(", "))
}

/// Convert a chat entry into one or more visual lines, splitting on `\n`.
///
/// Each entry type is delegated to its own submodule. Lines returned here are
/// content-width only - the gutter is rendered as a separate column.
///
/// Graphics-off: the adapters see an inactive `EntryGraphics` and emit
/// exactly the pre-graphics rendering. Prefer [`entry_to_lines_with_slots`]
/// from the render and layout paths — same lines when graphics are off,
/// image slot rows when they are on.
pub fn entry_to_lines(entry: &ChatEntry, ctx: &RenderContext) -> Vec<Line<'static>> {
    dispatch_to_lines(entry, ctx, &mut None)
}

/// [`entry_to_lines`], plus recording where image slot rows were emitted.
///
/// `slots_out` receives one [`ImageSlot`](jinn_chat_log_view_msg::ImageSlot)
/// per rendered image, carrying the slot's line index within this entry's
/// lines and its row count. The paint pass reads the records to draw the
/// actual image over the reserved rows.
///
/// Byte-identity contract: with graphics inactive (`ctx.graphics` carries no
/// rows), the output equals [`entry_to_lines`] exactly — the same property
/// the off-thread layout worker and the render pass both rely on, since both
/// go through this function with the same `EntryGraphics` snapshot.
pub fn entry_to_lines_with_slots(
    entry: &ChatEntry,
    ctx: &RenderContext,
    slots_out: &mut Vec<ImageSlot>,
) -> Vec<Line<'static>> {
    dispatch_to_lines(entry, ctx, &mut Some(slots_out))
}

/// The shared dispatch behind both entry points.
///
/// Graphics slot logic lives here rather than in the adapters on purpose:
/// both image-bearing entry kinds already place their image content at the
/// tail of their lines (the user block's trailing pad; the tool result's
/// summary line), so the dispatch can append or withhold slot rows without
/// any adapter knowing graphics exist. `entry_id` and `session_id` also
/// live here — the adapters never see them, keeping `to_lines` pure of
/// session state.
fn dispatch_to_lines(
    entry: &ChatEntry,
    ctx: &RenderContext,
    slots_out: &mut Option<&mut Vec<ImageSlot>>,
) -> Vec<Line<'static>> {
    match &entry.kind {
        ChatEntryKind::User {
            display,
            outcome,
            attachments,
            ..
        } => {
            let mut lines = user::to_lines(display, outcome, ctx);
            if ctx.graphics.active() {
                for (i, _attachment) in image_attachments(attachments) {
                    let rows = ctx.graphics.rows_for(i);
                    emit_slot_lines(
                        &mut lines,
                        ImageKey::new(&ctx.session_id, &entry.id, i),
                        rows,
                        Some(ctx.theme.user_block_bg),
                        ctx,
                        slots_out.as_deref_mut(),
                    );
                }
            }
            lines
        }
        ChatEntryKind::System(text) => system::to_lines(text, ctx),
        ChatEntryKind::Error(text) => error_entry::to_lines(text, ctx),
        ChatEntryKind::Actor { source, text } => actor::to_lines(source, text, ctx),
        ChatEntryKind::Assistant(text) => assistant::to_lines(text, ctx),
        ChatEntryKind::ToolCall {
            name, arguments, ..
        } => tool_call::to_lines(name, arguments, ctx),
        ChatEntryKind::ToolResult {
            name,
            content,
            status,
            truncation,
            is_alert,
            attachments,
            ..
        } => {
            // Graphics active: images render as slots, so the summary line
            // is withheld. Inactive: today's summary line, unchanged.
            let images = image_attachments(attachments);
            let summary = (!ctx.graphics.active())
                .then(|| attachments_summary(attachments))
                .flatten();
            let mut lines = tool_result::to_lines(
                name,
                content,
                *status,
                truncation.as_ref(),
                *is_alert,
                summary,
                ctx,
            );
            if ctx.graphics.active() {
                for (i, _attachment) in images {
                    let rows = ctx.graphics.rows_for(i);
                    emit_slot_lines(
                        &mut lines,
                        ImageKey::new(&ctx.session_id, &entry.id, i),
                        rows,
                        None,
                        ctx,
                        slots_out.as_deref_mut(),
                    );
                }
            }
            lines
        }
        ChatEntryKind::Thinking(text) => thinking::to_lines(text, ctx),
        ChatEntryKind::Annotation { citations } => annotation::to_lines(citations, ctx),

        ChatEntryKind::Transient(text) => transient::to_lines(text, ctx),
        ChatEntryKind::Compaction {
            summary,
            entries_compacted,
            tokens_before,
            tokens_after,
            ..
        } => compaction::to_lines(
            summary,
            *entries_compacted,
            *tokens_before,
            *tokens_after,
            ctx,
        ),
        ChatEntryKind::RuleInterrupt { rule, body } => rule_interrupt::to_lines(rule, body, ctx),
        ChatEntryKind::Intersession { text, .. } => intersession::to_lines(text, ctx),
    }
}

/// The image attachments in order, paired with their image-index (the index
/// into the image-only sequence, which is what `EntryGraphics.rows` and the
/// `ImageKey` attachment index count).
fn image_attachments(attachments: &[Attachment]) -> Vec<(usize, &Attachment)> {
    attachments
        .iter()
        .filter(|a| a.is_image())
        .enumerate()
        .collect()
}
