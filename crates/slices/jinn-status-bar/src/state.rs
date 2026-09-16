//! The status bar slice's owned state: the transient status hint.
//!
//! The payload type itself lives in `jinn-slices`
//! ([`jinn_status_bar_msg::StatusBarState`]) so the kernel's IntentHandler can
//! write it without depending on this crate; this module re-exports it
//! under the slice's name.

pub use jinn_status_bar_msg::StatusBarState;
pub use jinn_status_bar_msg::status_bar_slot;
