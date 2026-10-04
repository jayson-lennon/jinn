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
pub use shared::{GUTTER_WIDTH, RenderContext, strip_ansi};
pub use viewport::{ScrollState, compute_scroll, find_visible_indices};

use jinn_core_types::{ChatEntry, ChatEntryKind};
use ratatui::text::Line;

/// Convert a chat entry into one or more visual lines, splitting on `\n`.
///
/// Each entry type is delegated to its own submodule. Lines returned here are
/// content-width only - the gutter is rendered as a separate column.
pub fn entry_to_lines(entry: &ChatEntry, ctx: &RenderContext) -> Vec<Line<'static>> {
    match &entry.kind {
        ChatEntryKind::User {
            display, outcome, ..
        } => user::to_lines(display, outcome, ctx),
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
            ..
        } => tool_result::to_lines(name, content, *status, truncation.as_ref(), *is_alert, ctx),
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
    }
}
