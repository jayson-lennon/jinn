//! Chat log - renders the full conversation history.
//!
//! A display-only component showing all messages exchanged in the active session.
//! Each entry type has a distinct visual style (user bold with `>`, system dark gray,
//! actor yellow, assistant cyan). Supports scrolling, selection highlighting,
//! and pinned entry indicators.

pub(crate) mod actor;
pub(crate) mod annotation;
pub(crate) mod assistant;
pub mod audit_popup;
pub(crate) mod compaction;
pub(crate) mod error_entry;
pub(crate) mod history;
#[cfg(test)]
mod history_tests;
pub(crate) mod line_count_cache;
pub(crate) mod markdown;
pub(crate) mod shared;

pub(crate) mod system;
pub(crate) mod thinking;
pub(crate) mod tool_call;
pub(crate) mod tool_result;
pub(crate) mod transient;
pub(crate) mod user;
pub mod visual_item;

pub use audit_popup::format_audit_lines;
pub use history::ChatLogElement;
pub use history::entry_to_lines;
pub use shared::GUTTER_WIDTH;
pub use shared::RenderContext;
pub use shared::strip_ansi;

use crate::common::AppUiRegistry;

/// Register chat log UI element.
pub fn register(registry: &mut AppUiRegistry) {
    registry.register(Box::new(ChatLogElement::new()));
}
