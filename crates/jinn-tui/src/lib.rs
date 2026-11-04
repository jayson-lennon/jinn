//! jinn-tui: terminal user interface for the jinn agent harness.
//!
//! This crate provides the main event loop, terminal setup, rendering,
//! and key handling for the jinn TUI application.

pub mod app;
pub mod app_state;
pub mod clipboard_writer;
pub mod config;
pub mod convert;
pub mod keymap;
pub mod keymap_gen;
pub mod launch;
pub mod msg;
pub mod render;
pub mod run;
pub mod scope;
pub mod selection;
pub mod suspend;
pub mod terminal;
pub mod ui_elements;

pub use app::TuiApp;
pub use app::TuiAppBuilder;
pub use app_state::AppStatus;
pub use jinn_kernel::AppCore;
pub use jinn_kernel::Services;
pub use keymap::KeyCategory;
pub use launch::{LaunchError, launch, load_compaction_prompt, load_theme};
pub use msg::handler::MsgHandler;
pub use run::{TuiRunError, run};
pub use scope::Scope;
pub use ui_elements::build_ui_registry;

#[cfg(test)]
mod app_tests;

#[cfg(test)]
mod render_tests;

#[cfg(test)]
mod selection_tests;
