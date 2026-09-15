//! Picker - fuzzy search picker for providers, strategies, and sessions.
//!
//! Handles all picker intents (open, insert char, backspace, confirm, move,
//! cursor movement), their validators, and rendering.

pub mod action;
pub mod geometry;
pub mod host_impl;
pub mod intent;
pub mod mcp_server_spec;
pub mod persona_spec;
pub mod picker_kind;
pub mod plugin_spec;
pub mod reasoning_effort_spec;
pub mod registry;
pub mod render;
pub mod session_lifecycle_spec;
pub mod skill_spec;
pub mod task_list_spec;
pub mod theme_spec;
pub mod tool_spec;

pub mod style;
pub mod validator;

pub use picker_kind::PickerKind;
