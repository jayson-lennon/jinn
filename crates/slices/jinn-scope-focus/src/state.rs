//! The scope-focus slice's owned state.
//!
//! The payload type lives in `jinn-slices`
//! ([`jinn_slices::ScopeFocusState`]) so the kernel writes it without
//! depending on this crate; this module re-exports it under the
//! slice's name.

pub use jinn_slices::ScopeFocusState;
