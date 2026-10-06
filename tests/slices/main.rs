//! Slice-composition integration tests.
//!
//! These exercise the composed system — real slice activation over the
//! kernel's registries, real route rows, real cells and actors — the
//! layer no individual crate can test in isolation. They live here (the
//! root crate's `tests/`) so `just check` and IDE analysis never compile
//! slice crates into the kernel or the tui crate graph.
//!
//! One test binary, one file per slice (plus the composition seam), each
//! mirroring the slice it tests:
//!
//! - [`discord`] — discord's rows in the composed keymap
//! - [`quake_bar`] — quake-bar's rows in the composed keymap
//! - [`dashboard`] — dashboard's scope, cell, actor, rendering
//! - [`provider_selection`] — provider switch / model discovery / picker load
//! - [`session_init`] — the discovery chain (kernel → trouper →
//!   keyed worker → per-resource Loaded events)
//! - [`chat_log`] — the chat log's rows in the composed keymap
//! - [`composition`] — the shared seam (every slice's rows attached)
//!
//! Coverage split:
//! - tui unit tests: synthetic/slice-shaped inputs only
//! - slice crate tests: each slice's own row shape and behavior
//! - these tests: the composition of both (keys resolve across the
//!   built-in keymap plus every slice's rows; rendering against real
//!   slice cells; actors applying routed messages).

#![allow(clippy::expect_used, clippy::panic, reason = "test code")]

mod chat_log;
mod common;

mod boot;
mod boot_list;
mod chat_input;
mod composition;
mod dashboard;
mod discord;
mod phase_control;
mod preferences;
mod project;
mod provider_selection;
mod quake_bar;
mod session_init;
mod session_lifecycle;
mod sidebar;
mod watchdog;
