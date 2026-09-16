//! Shared path-display transforms.
//!
//! The transform moved to `jinn-slices` (the cwd slice and the status-bar
//! slice speak it); re-exported here for the kernel's existing import paths.

pub use jinn_slices::shorten_path;
