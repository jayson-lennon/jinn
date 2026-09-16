//! Sessions-list model — the ordered, tree-shaped view of loaded
//! sessions that the sessions sidebar section renders.
//!
//! The kernel owns this model: it reads the session map and the sidebar
//! sections cell, builds [`SessionEntry`] rows in display order, and
//! reconciles cursors and visual-parent links when sessions are removed
//! or loaded. The sidebar slice drives its section interactions on top
//! of these functions through its kernel dependency.
pub mod archive_tree;
pub mod close;
pub mod load_subagent;
pub mod reconcile;
pub mod state;
