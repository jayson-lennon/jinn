//! Curated project directories - the list surfaced by the project picker.
//!
//! A "project" is simply a directory the user wants to keep on file so they can
//! spin up a new session rooted there in one step (see the project picker, bound
//! to `<leader>so`). Unlike an auto-tracked MRU, this list is purely curated: the
//! user adds and removes entries explicitly, so it never drifts with usage.
//!
//! Defined in `jinn.toml` under `[[project]]` and persisted comment-preserving
//! via the [`DocumentPatcher`](crate::common::toml_patch::DocumentPatcher).

pub mod picker_entry;
pub mod resolver;

pub use jinn_preferences_config::schemas::ProjectConfig;
