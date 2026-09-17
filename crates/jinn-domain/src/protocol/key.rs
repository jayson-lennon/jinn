//! Key representation for keyboard events.
//!
//! The types live in [`jinn_slices::key`] so slice crates (which cannot
//! depend on `jinn-domain`) can share the same keyboard vocabulary;
//! this module is a re-export shim for kernel code paths.

pub use jinn_slices::key::{Key, KeyEvent, Modifiers};
