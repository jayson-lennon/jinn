//! Contracts for the chat log view slice.
//!
//! The rendered-lines cache lives here rather than in the slice because
//! `AppState` holds one: the kernel must depend on a slice's message
//! crate, never on its implementation crate.

pub mod audit_popup_state;
pub mod chat_log_view_state;
pub mod graphics;
pub mod layout;
pub mod line_cache_cell;
pub mod line_count_cache;
pub mod scope;
pub mod visual_item;

pub use audit_popup_state::{AuditPopupState, audit_popup_slot};
pub use chat_log_view_state::*;
pub use graphics::{GraphicsState, ImageKey, ImageSlot, SlotGeometry, chat_log_graphics_slot};
pub use layout::{
    ArmLayoutDeadline, ArmPreviewDeadline, ChatLogLayoutComputed, Escalated, LayoutChatSession,
    LayoutDeadlineExpired, MeasuredEntryCount, PREVIEW_ENTRY_COUNT, PREVIEW_MARKER_COLUMNS,
    PREVIEW_MARKER_MAX_ROWS, PREVIEW_MAX_LINES, PREVIEW_REQUEST_ENTRY_COUNT,
    PreviewDeadlineExpired, PreviewSessionRequested, SessionPreviewRendered, entry_is_settled,
};
pub use line_cache_cell::entry_line_cache_slot;
pub use line_count_cache::{
    CacheHit, CacheProbe, CachedEntryCount, ContentIdentity, EntryLineCache,
    MAX_CACHED_RENDERED_ENTRIES, MeasuredLineCount,
};
pub use scope::{IGNORE_SELECTED_ACTION, chat_log_scope};
pub use visual_item::{
    DEFAULT_MIN_COLLAPSE_COUNT, PROXIMITY_COUNT, VisualItem, build_visual_items,
    entry_id_from_visual_item, resolve_entry_id_to_vi_index,
};
